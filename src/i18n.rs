// src/i18n.rs
//!
//! 轻量 i18n：从 `locales/*.ftl` 读取 Fluent 资源，按「精确 locale → 主语言 → en-US」
//! 逐级回退（spec §5.1）。整条链都取不到时返回 key 本身——这样即使翻译缺失，
//! 界面也只会露出 key，而不会 panic 或输出空白。

use crate::debug;
// 用 `concurrent::FluentBundle`（memoizer 内部是 Mutex）而不是默认的
// `FluentBundle`（内部是 RefCell）：只有前者是 `Send + Sync`，而下面的进程级
// `OnceLock<I18nManager>` 要求它。默认那个连 `static` 都放不进去——这不是
// 「换个写法」的偏好，是编译期硬性条件。两者的格式化结果完全一致。
use fluent::concurrent::FluentBundle;
use fluent::{FluentResource, FluentValue};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use unic_langid::{langid, LanguageIdentifier};

/// 回退链的最后一格。
const FALLBACK_LOCALE: &str = "en-US";

/// 资源目录。这里按当前工作目录解析，与 CLI 从仓库根运行的既有约定一致；
/// 不做资源内嵌或路径探测——那不在本计划范围内。
const LOCALES_DIR: &str = "locales";

/// i18n 初始化失败的原因。
///
/// 这里只有「资源本身坏了」才叫失败。以下两种情况都**不算失败**：
/// `locales/` 目录或某个具体 `.ftl` 不存在（这一级回退没内容，链路继续往下走），
/// 以及语言标签解析不了（退回 en-US，见 [`I18nManager::init`]）。
#[derive(Debug)]
pub enum I18nError {
    /// `.ftl` 存在但语法不合法
    FtlParse { path: PathBuf, errors: Vec<String> },
    /// `.ftl` 存在但读不出来
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for I18nError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FtlParse { path, errors } => write!(
                f,
                "语言文件 {} 解析失败: {}",
                path.display(),
                errors.join("; ")
            ),
            Self::Io { path, source } => {
                write!(f, "读取语言文件 {} 失败: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for I18nError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// 按优先级持有多级语言资源。
///
/// 刻意用「有序 Vec」而不是把所有资源合进一个 `FluentBundle`：`add_resource`
/// 撞上重复消息 ID 会直接报错，`add_resource_overriding` 又会把后加入的资源
/// 提到最高优先级——两者都会破坏回退顺序。显式逐级查找才能让「精确命中优先于
/// en-US」这件事可被测试。
pub struct I18nManager {
    bundles: Vec<FluentBundle<FluentResource>>,
}

impl I18nManager {
    /// 按「配置 → 系统 locale → en-US」确定语言并加载各级资源（spec §4）。
    ///
    /// 语言标签会先规范化成 BCP 47（`en_US` → `en-US`，`zh_cn` → `zh-CN`），
    /// 后续的文件名和回退链都基于规范化后的结果——这样配置里写 POSIX 风格或
    /// 大小写不规范的标签也能命中同一个文件。
    ///
    /// 标签解析不了**不阻断启动**：`LANG=C` / `LC_ALL=C` 是 Docker、CI、cron 的
    /// 常见默认值，一个命令行工具不该因为读不懂一个语言串就拒绝运行。这时退回
    /// en-US，也就是 spec §4 那条链的最后一格。
    pub fn init(config_language: Option<String>) -> Result<Self, I18nError> {
        let requested = config_language
            .filter(|language| !language.trim().is_empty())
            .or_else(sys_locale::get_locale)
            .unwrap_or_else(|| FALLBACK_LOCALE.to_string());

        let locale = resolve_locale(&requested);

        let mut bundles = Vec::new();
        for (stem, bundle_locale) in fallback_chain(&locale) {
            if let Some(bundle) = load_bundle(&stem, &bundle_locale)? {
                bundles.push(bundle);
            }
        }

        Ok(Self { bundles })
    }

    /// 取一条本地化消息；`args` 用于填充消息里的 `{ $var }`。
    ///
    /// 沿回退链找第一个**含有该 key**的资源；只有属性、没有 value 的消息视为
    /// 不存在。整个链都没有就返回 key 本身。
    pub fn get_message(&self, key: &str, args: Option<&HashMap<String, FluentValue>>) -> String {
        let args = args.map(to_fluent_args);

        for bundle in &self.bundles {
            let Some(message) = bundle.get_message(key) else {
                continue;
            };
            let Some(pattern) = message.value() else {
                continue;
            };

            // 格式化期的错误（例如调用方漏传了某个 `$var`）不改写「先命中先返回」
            // 的语义：消息本身在，就把它的渲染结果交出去。
            let mut errors = Vec::new();
            return bundle
                .format_pattern(pattern, args.as_ref(), &mut errors)
                .into_owned();
        }

        key.to_string()
    }
}

/// 进程级实例：`main()` 安装一次，之后所有调用点共用（R8）。
static MANAGER: OnceLock<I18nManager> = OnceLock::new();

/// 还没安装时的兜底管理器。
///
/// 用一个独立的空管理器而不是 `OnceLock::get_or_init`：后者会在第一次 `t()` 时就把
/// 空管理器**钉死**，之后 `install` 永远装不进去——一次早于安装的调用就能让整个进程
/// 只输出 key，而且毫无征兆。这里 `get` 失败只是回落，安装仍然随时可以生效。
static UNINSTALLED: I18nManager = I18nManager {
    bundles: Vec::new(),
};

/// 安装进程级 i18n。`main()` 里在**任何输出之前**调用一次。
///
/// 语言资源坏了不阻断启动：`init` 失败就装一个空管理器，界面露出 key，
/// 其余功能照常走。locale 的问题从来不是致命的（spec §4）。
pub fn install(config_language: Option<String>) {
    let manager = match I18nManager::init(config_language) {
        Ok(manager) => manager,
        Err(err) => {
            // 整片界面都会变成 key，这种程度的问题至少该在 --debug 下留一句原因。
            if debug::enabled() {
                eprintln!("[debug] i18n: {err}");
            }
            I18nManager {
                bundles: Vec::new(),
            }
        }
    };

    // set-once：后到的安装被忽略。一个进程只有一种界面语言，这正是想要的语义。
    let _ = MANAGER.set(manager);
}

/// 取一条本地化消息。
///
/// 没安装过（测试，或早于 `main()` 安装的路径）时返回 key 本身，不会 panic。
pub fn t(key: &str) -> String {
    manager().get_message(key, None)
}

/// 带插值的版本：`t_args("save_config_error", &[("error", e.to_string().into())])`。
///
/// 参数收 `&[(&str, FluentValue)]` 而不是 `&HashMap`：调用点全是「一两个具名变量」，
/// 让每处都搭一个 map 是纯噪声。类型不是 `FluentValue` 的值先 `to_string()`。
pub fn t_args<'v>(key: &str, args: &[(&str, FluentValue<'v>)]) -> String {
    let args: HashMap<String, FluentValue<'v>> = args
        .iter()
        .map(|(name, value)| ((*name).to_string(), value.clone()))
        .collect();

    manager().get_message(key, Some(&args))
}

/// 当前生效的管理器。
///
/// `OnceLock<I18nManager>` 要求 `I18nManager: Send + Sync`（fluent 的 bundle 内部
/// 把 memoizer 放在锁里，这两个 bound 成立）；不成立的话这里根本编译不过，
/// 所以不存在「悄悄退化成 thread_local」的余地——那会在多线程 tokio 运行时下出错。
fn manager() -> &'static I18nManager {
    MANAGER.get().unwrap_or(&UNINSTALLED)
}

/// 把配置或系统给出的语言串解析成 locale。
///
/// 解析不了就退回 en-US（spec §4 那条链的最后一格）。**配置值走这里，系统检测值
/// 也走这里**——一个命令行工具不该因为读不懂一个语言串就拒绝运行。
fn resolve_locale(requested: &str) -> LanguageIdentifier {
    requested
        .trim()
        .parse()
        .unwrap_or_else(|_| langid!("en-US"))
}

/// 回退链：精确 locale → 主语言 → en-US，按序去重（`en-US` 自己只需查一遍）。
///
/// 每一格是 `(文件名词干, 该文件的 locale)`——两者分开，是因为语言级那一格的文件名
/// 取主语言（`fr-CA` → `fr.ftl`），而 bundle 的 locale 只影响复数与日期规则，
/// 同一个语言下并无差别，直接复用不必再解析出一个 `LanguageIdentifier` 来。
fn fallback_chain(locale: &LanguageIdentifier) -> Vec<(String, LanguageIdentifier)> {
    let mut chain = vec![(locale.to_string(), locale.clone())];

    let language = locale.language.to_string();
    if language != chain[0].0 {
        chain.push((language, locale.clone()));
    }
    if !chain.iter().any(|(stem, _)| stem == FALLBACK_LOCALE) {
        chain.push((FALLBACK_LOCALE.to_string(), resolve_locale(FALLBACK_LOCALE)));
    }

    chain
}

/// 加载一个文件名词干对应的 `.ftl`。
///
/// 文件不存在返回 `Ok(None)`——这一级回退空缺，让链路继续往下走。
fn load_bundle(
    stem: &str,
    locale: &LanguageIdentifier,
) -> Result<Option<FluentBundle<FluentResource>>, I18nError> {
    let path = Path::new(LOCALES_DIR).join(format!("{stem}.ftl"));
    if !path.is_file() {
        return Ok(None);
    }

    let source = std::fs::read_to_string(&path).map_err(|source| I18nError::Io {
        path: path.clone(),
        source,
    })?;

    let resource = FluentResource::try_new(source).map_err(|(_, errors)| I18nError::FtlParse {
        path: path.clone(),
        errors: errors.iter().map(ToString::to_string).collect(),
    })?;

    let mut bundle = FluentBundle::new_concurrent(vec![locale.clone()]);
    // 关掉双向文本隔离符（FSI/PDI）：那是给 HTML 用的，落到终端里既看不见，
    // 又会让字符串比较、`grep` 和管道下游解析凭空多出不可见字符。
    bundle.set_use_isolating(false);

    bundle
        .add_resource(resource)
        .map_err(|errors| I18nError::FtlParse {
            path: path.clone(),
            errors: errors.iter().map(ToString::to_string).collect(),
        })?;

    Ok(Some(bundle))
}

/// 把调用方的参数表转成 Fluent 自己的 `FluentArgs`。
///
/// 键选择 clone 而不是借用 `&str`：借用会要求参数表本身的生存期不短于参数值的
/// 生存期，而 `get_message` 的签名把这两者都省略了，借用在泛型下无法通过借用检查。
fn to_fluent_args<'v>(args: &HashMap<String, FluentValue<'v>>) -> fluent::FluentArgs<'v> {
    args.iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn test_hello_en() {
        let manager = I18nManager::init(Some("en-US".to_string())).unwrap();
        let mut args = HashMap::new();
        args.insert("name".to_string(), fluent::FluentValue::from("World"));

        let msg = manager.get_message("hello", Some(&args));
        assert_eq!(msg, "Hello, World!");
    }

    #[test]
    fn test_hello_fr_fallback_en() {
        // Use a locale that doesn't exist to test fallback to en-US
        let manager = I18nManager::init(Some("es-ES".to_string())).unwrap();
        let mut args = HashMap::new();
        args.insert("name".to_string(), fluent::FluentValue::from("World"));

        let msg = manager.get_message("hello", Some(&args));
        // Since es-ES.ftl doesn't exist, it should fallback to en-US.ftl
        assert_eq!(msg, "Hello, World!");
    }

    #[test]
    fn test_hello_fr_exact() {
        let manager = I18nManager::init(Some("fr-US".to_string())).unwrap();
        let mut args = HashMap::new();
        args.insert("name".to_string(), fluent::FluentValue::from("Monde"));

        let msg = manager.get_message("hello", Some(&args));
        assert_eq!(msg, "Bonjour, Monde!");
    }

    /// 回退链的中间一级：`fr-FR.ftl` 不存在，但 `fr.ftl` 存在，必须命中它
    /// 而不是直接掉到 en-US。这条是「有序 Vec 逐级查找」这个设计决策的唯一覆盖。
    #[test]
    fn test_language_level_fallback() {
        let manager = I18nManager::init(Some("fr-FR".to_string())).unwrap();
        let mut args = HashMap::new();
        args.insert("name".to_string(), fluent::FluentValue::from("Monde"));

        let msg = manager.get_message("hello", Some(&args));
        assert_eq!(msg, "Bonjour, Monde!");
    }

    /// 回归测试：`LANG=C` / `LC_ALL=C`（Docker、CI、cron 的常见默认值）给出的
    /// 不是 BCP 47 标签，绝不能因此拒绝启动——退回 en-US，UI 走英文那一格。
    #[test]
    fn test_unparseable_locale_falls_back_to_en_us() {
        let manager =
            I18nManager::init(Some("C".to_string())).expect("locale 解析失败不应让 init 失败");
        let mut args = HashMap::new();
        args.insert("name".to_string(), fluent::FluentValue::from("World"));

        let msg = manager.get_message("hello", Some(&args));
        assert_eq!(msg, "Hello, World!");
    }

    #[test]
    fn test_missing_key() {
        let manager = I18nManager::init(Some("en-US".to_string())).unwrap();
        let msg = manager.get_message("non_existent_key", None);
        assert_eq!(msg, "non_existent_key");
    }

    /// 全局访问器建立在 `OnceLock<I18nManager>` 上，这要求管理器是 `Send + Sync`
    /// （R8 的前置条件）。把它钉在编译期：若哪天 fluent 换成 `!Sync` 的内部结构，
    /// 这里先报错，而不是等运行时才暴露。
    #[test]
    fn manager_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<I18nManager>();
    }

    /// 没安装过时的契约：吐 key，不 panic。
    ///
    /// 这里测的是 `manager()` 的回退对象本身——「先于安装调用」在单进程内造不出来
    /// （一旦有测试装了管理器，全局就是装过的状态），所以直接钉住它的行为。
    #[test]
    fn uninstalled_manager_returns_the_key() {
        assert_eq!(
            UNINSTALLED.get_message("not_git_repo", None),
            "not_git_repo"
        );
    }

    /// 固定键：`zh-CN.ftl` 必须能解析，且真实键渲染出中文原文。
    ///
    /// 在此之前 **没有任何测试加载过 zh-CN**（`cargo test` 只初始化 en-US / fr /
    /// fr-US / es-ES / C），也就是说中文资源里写坏一个值是可以一路发布的。
    #[test]
    fn zh_cn_resource_parses_and_renders_chinese() {
        let manager =
            I18nManager::init(Some("zh-CN".to_string())).expect("locales/zh-CN.ftl 必须能解析");

        // 普通一条
        assert_eq!(
            manager.get_message("not_git_repo", None),
            "❌ 当前目录不是 Git 仓库"
        );
        assert_eq!(manager.get_message("commit_success", None), "✅ 提交成功！");

        // 带插值的（下标是 usize）
        let mut args = HashMap::new();
        args.insert("index".to_string(), FluentValue::from(1usize));
        args.insert("total".to_string(), FluentValue::from(3usize));
        assert_eq!(
            manager.get_message("chunk_summary", Some(&args)),
            "第 1/3 块摘要"
        );

        // 前导空格靠 {"   "} 占位符带出来，Fluent 自己会吃掉行首空白
        assert_eq!(
            manager.get_message("use_all_flag_hint", None),
            "   或使用 --all 参数包含所有变更"
        );
    }

    /// zh/en 键集对齐：zh 里的每个键都必须在 en-US 里存在。
    ///
    /// 少一个键不会报错，只会在运行时悄悄回退成 key 本身——所以这条要在测试里拦。
    /// 断言是**单向**的：`hello` 只存在于 en-US（R3 的固定测试键），
    /// 差集必须恰好是它，en 侧多出来的别的键同样是漂移。
    #[test]
    fn zh_cn_keys_are_all_present_in_en_us() {
        let zh = I18nManager::init(Some("zh-CN".to_string())).unwrap();
        let en = I18nManager::init(Some("en-US".to_string())).unwrap();

        let zh_keys = ftl_keys(&read_locale("zh-CN"));
        let en_keys = ftl_keys(&read_locale("en-US"));

        let undefined: Vec<&String> = zh_keys.iter().filter(|key| !defines(&en, key)).collect();
        assert!(
            undefined.is_empty(),
            "zh-CN 有、en-US 没有的键（运行时会露出 key）: {undefined:?}"
        );

        let en_only: Vec<&String> = en_keys.difference(&zh_keys).collect();
        assert_eq!(
            en_only,
            vec!["hello"],
            "en-US 独有的键应当只剩 R3 的 hello 固定键"
        );

        // 反向自证：扫出来的键在 zh 上也必须定义得动，否则上面两条可能只是扫描器坏了
        let zh_undefined: Vec<&String> = zh_keys.iter().filter(|key| !defines(&zh, key)).collect();
        assert!(
            zh_undefined.is_empty(),
            "zh-CN 里解析不出的键: {zh_undefined:?}"
        );
    }

    /// 某个键在回退链里是否**有定义**。
    ///
    /// 这里不能用「渲染结果 != key」当判据：`en-US.ftl` 的 `received = received`
    /// 值恰好就等于键本身，会把一个完好的条目误判成缺失。`has_message` 查的是
    /// 条目本身，与 `get_message` 的查找口径一致。
    fn defines(manager: &I18nManager, key: &str) -> bool {
        manager.bundles.iter().any(|bundle| bundle.has_message(key))
    }

    fn read_locale(locale: &str) -> String {
        std::fs::read_to_string(format!("{LOCALES_DIR}/{locale}.ftl"))
            .unwrap_or_else(|err| panic!("读不到 locales/{locale}.ftl: {err}"))
    }

    /// 从 `.ftl` 源文本里扫出顶层消息 ID。
    ///
    /// 为什么不用解析器：`FluentResource::entries()` 的 item 类型是
    /// `fluent_syntax::ast::Entry`，而 `fluent-syntax` 没有被 fluent-bundle re-export，
    /// 要匹配变体就得为一个测试再引一个必须与 fluent-bundle 版本严格同步的直接依赖。
    /// 这里扫的是本仓库自己维护的 `locales/*.ftl`，格式规整（键在行首、`#` 是注释、
    /// 多行值必须缩进），漏扫的只可能是注释和空行；真漏了一个键，上面「zh 有 en 没有」
    /// 那条会以别的方式炸出来。
    fn ftl_keys(source: &str) -> BTreeSet<String> {
        source
            .lines()
            .filter(|line| !line.starts_with(char::is_whitespace) && !line.starts_with('#'))
            .filter_map(|line| line.split_once('='))
            .map(|(id, _)| id.trim().to_string())
            .filter(|id| !id.is_empty())
            .collect()
    }
}
