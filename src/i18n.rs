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
        let requested = requested_locale(config_language, sys_locale::get_locale());

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

/// 测试固定 locale（R7）。
///
/// 断言的是「按 zh-CN 渲染出的那句话」时，必须先把进程 locale 钉死：`init(None)`
/// 会去读宿主机的系统 locale，结果随机器而变。`OnceLock` 是 set-once 的，而一个
/// 测试二进制只有一个进程、多个线程——所以全局只能钉一种语言：所有调用方都请求
/// zh-CN，先到的那个赢，与线程调度无关；若哪个测试改请求别的语言，套件立刻变成
/// 顺序相关的。
#[cfg(test)]
pub(crate) fn pin_test_locale() {
    install(Some("zh-CN".to_string()));
}

/// spec §4 的优先级：配置值 → 系统 locale → en-US。
///
/// 「检测」这一步作为参数传进来（`init` 传 `sys_locale::get_locale()`），而不是
/// 在函数体里直接调用：不这样抽，第三格就只能靠「断言 `init(None)` 的结果和
/// `init(sys_locale::get_locale())` 的结果相等」间接验证，而宿主机恰好就是期望
/// 语言时那条对拍恒真——等于没测。抽出来之后三级优先级可以逐格钉死。
///
/// 空白配置按「没配」处理：`"language": "  "` 若被当成有效值送进 [`resolve_locale`]，
/// 会解析失败退回 en-US，把系统 locale 这一格整个盖掉。
fn requested_locale(config_language: Option<String>, detected: Option<String>) -> String {
    config_language
        .filter(|language| !language.trim().is_empty())
        .or(detected)
        .unwrap_or_else(|| FALLBACK_LOCALE.to_string())
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

    /// locale 选择的优先级，逐格对齐 spec §4：配置值 → 系统 locale → en-US。
    ///
    /// 这一格（以及下一条）是 Phase 4 里「locale selection」那半：`init` 的两端都被
    /// 真实 `.ftl` 覆盖着（`test_hello_*` / `test_unparseable_locale_falls_back_to_en_us`
    /// 走的是完整链路），这里补的是**三级之间谁压谁**——尤其是空白配置这一格，
    /// 它必须等于「没配」，否则会盖掉系统 locale。
    #[test]
    fn locale_selection_follows_the_spec_priority() {
        // 1. 配了就用配置的，系统 locale 插手不了
        assert_eq!(
            requested_locale(Some("zh-CN".to_string()), Some("fr-FR".to_string())),
            "zh-CN"
        );
        // 2. 没配才轮到系统 locale
        assert_eq!(requested_locale(None, Some("fr-FR".to_string())), "fr-FR");
        // 3. 空白配置等于没配
        assert_eq!(
            requested_locale(Some("   ".to_string()), Some("fr-FR".to_string())),
            "fr-FR"
        );
        // 4. 系统 locale 也给不出东西时落到链尾
        assert_eq!(requested_locale(None, None), "en-US");
    }

    /// 不可用的语言串落到 en-US，不阻断启动。
    ///
    /// `requested_locale` 只挑出字符串，能不能变成一个 locale 由 `resolve_locale`
    /// 决定——两段合起来才是 spec §4 那条链的最后一格。配置值与系统检测值走的是
    /// 同一个入口，所以两边的落点都必须一致。
    #[test]
    fn unusable_language_strings_land_on_en_us() {
        assert_eq!(
            resolve_locale(&requested_locale(Some("C".to_string()), None)).to_string(),
            "en-US"
        );
        assert_eq!(
            resolve_locale(&requested_locale(None, Some("C".to_string()))).to_string(),
            "en-US"
        );
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

    /// 所有对空白敏感的键，逐字节对齐原字面量。
    ///
    /// 这是本次改造里最容易**静默**出错的地方：Fluent 会吃掉值的行首、行尾空白，
    /// 所以前导空格得写成 `{"   "}`、结尾空格得写成 `{" "}`。写错不会有任何报错，
    /// 只是终端里少几个空格、或者用户敲 y 时提示语贴着光标。上面那条测试只覆盖了
    /// 其中一个键，这里把剩下的全部钉住。
    ///
    /// 期望值直接抄自 key-manifest 的 original literal 列（原字面量逐字拷贝）；
    /// 带插值的键比的是**渲染结果**，不是 `{ $count }` 这样的原始模式——后者会让
    /// 断言恒真（`{ $count } != 3` 永远成立）。
    ///
    /// 覆盖面的定义写清楚，免得这条测试再被当成它没做到的东西：**值里带首尾空格、
    /// 或含连续两个及以上空格的键**（`{"  "}` 这种占位符按它代表的空格算）。
    /// `locales/zh-CN.ftl` 与 `locales/en-US.ftl` 里各 28 个，键名一一对应，
    /// 本测试把 28 个**全部**钉住——所以这一类是完备覆盖，不是「挑了几个重要的」。
    /// 复核方式：`grep -cE '=.*(  |\{"[ ]*"\})' locales/zh-CN.ftl` 得 27，
    /// 余下 1 个是 `no_content_error`，它那两个空格写在**续行**上（第 100 行）。
    /// 末尾三条是**反向**属性——「值首尾不能有空格」——它们的值里本来就没有空格，
    /// 因此不在这 28 个里，单独钉是为了另一件事（见那段注释）。
    ///
    /// 一点边界，之前写成「只有中文这一格会中招」，那句是错的：`locales/en-US.ftl`
    /// 里同样有 `⚠️` 后面两个空格的值，也同样走那几处 println!，在英文值里多写一个
    /// `{" "}` 会把空格翻倍得一模一样。准确的说法是「本守卫目前只读 **zh-CN** 这一格」。
    /// 两个文件的**键集**由 `zh_cn_keys_are_all_present_in_en_us` 守，
    /// **值的空白形状只钉了 zh-CN**；要连 en-US 一起钉，得让本测试对两个 manager
    /// 各跑一遍——那件事没做，别以为做了。
    #[test]
    fn whitespace_sensitive_keys_match_their_original_literals() {
        let manager = I18nManager::init(Some("zh-CN".to_string())).unwrap();
        let args = |pairs: &[(&str, FluentValue<'static>)]| {
            pairs
                .iter()
                .map(|(name, value)| ((*name).to_string(), value.clone()))
                .collect::<HashMap<String, FluentValue<'static>>>()
        };

        // 三个前导空格的提示行
        assert_eq!(
            manager.get_message("use_all_flag_hint", None),
            "   或使用 --all 参数包含所有变更"
        );
        assert_eq!(
            manager.get_message("strict_format_blocked_hint", None),
            "   可先用 --dry-run 预览，或在 config.json 中设置 strict_format=false"
        );
        assert_eq!(
            manager.get_message("debug_hint", None),
            "   提示: 加 --debug 查看完整原始响应，或在 config.json 中调大 max_tokens"
        );
        assert_eq!(
            manager.get_message("block_details", Some(&args(&[("blocks", "3, 5".into())]))),
            "   降级的块: 3, 5"
        );
        assert_eq!(
            manager.get_message(
                "chunk_progress",
                Some(&args(&[("index", 1usize.into()), ("total", 3usize.into())]))
            ),
            "   [块 1/3] 正在生成摘要..."
        );

        // 一个前导空格
        assert_eq!(manager.get_message("chunk_complete", None), " 完成");
        assert_eq!(
            manager.get_message("chunk_degraded", None),
            " ⚠️ 已降级为本地结构化摘要"
        );

        // `⚠️` 后面是两个空格——不是一个，也不是三个
        assert_eq!(
            manager.get_message("format_validation_failed", None),
            "⚠️  格式验证失败，请检查"
        );
        assert_eq!(
            manager.get_message(
                "validation_issues",
                Some(&args(&[("issue", "消息为空".into())]))
            ),
            "⚠️  消息为空"
        );

        // 结尾空格是值的一部分：用户在提示语后面直接敲 y/n/e
        assert_eq!(
            manager.get_message("edit_message_prompt", None),
            "是否使用此消息提交？(y/n/e 编辑): "
        );

        // 结尾的 `\n` **不是**值的一部分，留在调用处（main.rs 的 format! 里补）
        assert_eq!(
            manager.get_message(
                "fallback_more_files",
                Some(&args(&[("count", 1usize.into())]))
            ),
            "- …（其余 1 个文件略）"
        );

        // 多行值：Fluent 会剥掉多行值的行首缩进，`chore:` 后面那个空行必须还在
        assert_eq!(
            manager.get_message(
                "fallback_message",
                Some(&args(&[
                    ("count", 2usize.into()),
                    ("body", "- M  a.rs\n".into())
                ]))
            ),
            "chore: 更新 2 个文件\n\n- M  a.rs\n（本条消息由本地降级逻辑生成：模型未返回内容）"
        );

        // 同一属性的其余六条：`⚠️` 后面同样是**两个**空格。值里写成一个，
        // 终端上就是 `⚠️ 格式...`，同样不报错。
        assert_eq!(
            manager.get_message("format_warning_strict", None),
            "⚠️  strict_format = true，但当前是交互模式，是否提交由你决定"
        );
        assert_eq!(
            manager.get_message("format_warning_nonstrict", None),
            "⚠️  格式校验未通过（strict_format = false，仍可提交）"
        );
        assert_eq!(
            manager.get_message("degraded_confirmation_required", None),
            "⚠️  本次为降级消息（模型未正常返回），强制走确认流程"
        );
        assert_eq!(
            manager.get_message("long_diff_warning", None),
            "⚠️  变更内容较长，正在采用“分块总结 -> 合并 -> 生成”机制进行处理..."
        );
        assert_eq!(
            manager.get_message(
                "degradation_warning",
                Some(&args(&[("count", 1usize.into()), ("total", 3usize.into())]))
            ),
            "⚠️  有 1/3 块使用了本地降级摘要，最终消息质量可能下降"
        );
        assert_eq!(
            manager.get_message(
                "final_generation_failed",
                Some(&args(&[("error", "连接超时".into())]))
            ),
            "⚠️  最终生成失败，改用本地兜底消息：连接超时"
        );

        // ── ai.rs 接进来之后补上的那一批 ────────────────────────────────────
        // `report()` 的每一行都以两个空格缩进，两条诊断/提示行也是。这些键以前是
        // 硬编码字面量，不在上面的范围里；ai.rs 改走 t()/t_args() 之后它们上了真实
        // 界面，于是必须一起钉住——否则「守卫盖住了这一类」这句话在 ai.rs 落地的那一刻
        // 就不再成立。
        assert_eq!(
            manager.get_message(
                "report_endpoint",
                Some(&args(&[
                    ("endpoint", "http://127.0.0.1:1/v1".into()),
                    ("model", "test-model".into())
                ]))
            ),
            "  端点: http://127.0.0.1:1/v1   模型: test-model"
        );
        // 温度按**渲染后的字符串**传（`report()` 里就是 `.to_string()`）：Fluent 内部
        // 把数值统一走 f64，`0.7f32` 会渲染成 `0.699999988079071`。这里若写成
        // `0.7f32.into()`，这条断言就和真实调用点脱节了。
        assert_eq!(
            manager.get_message(
                "report_request",
                Some(&args(&[
                    ("max_tokens", 64u32.into()),
                    ("temperature", "0.7".into()),
                    ("status", 200u16.into())
                ]))
            ),
            "  请求: max_tokens=64 temperature=0.7   HTTP 200"
        );
        assert_eq!(
            manager.get_message(
                "report_frames",
                Some(&args(&[
                    ("total", 3usize.into()),
                    ("data", 2usize.into()),
                    ("heartbeat", 1usize.into()),
                    ("parse_failures", 0usize.into())
                ]))
            ),
            "  帧: 共 3（data 2 / 其他 1）  解析失败 0"
        );
        assert_eq!(
            manager.get_message(
                "report_content_frames",
                Some(&args(&[
                    ("content_frames", 1usize.into()),
                    ("content_chars", 4usize.into()),
                    ("reasoning_frames", 2usize.into()),
                    ("reasoning_chars", 9usize.into())
                ]))
            ),
            "  含 content 的帧 1（4 字）   含推理内容的帧 2（9 字）"
        );
        assert_eq!(
            manager.get_message(
                "report_finish_reason",
                Some(&args(&[
                    ("reason", "length".into()),
                    ("done", "已收到".into())
                ]))
            ),
            "  finish_reason: length   [DONE]: 已收到"
        );
        assert_eq!(
            manager.get_message(
                "report_stream_error",
                Some(&args(&[("error", "{\"code\":503}".into())]))
            ),
            "  流内错误: {\"code\":503}"
        );
        assert_eq!(
            manager.get_message(
                "report_diagnosis",
                Some(&args(&[(
                    "diagnosis",
                    "网关在流中返回了 error 对象".into()
                )]))
            ),
            "  诊断: 网关在流中返回了 error 对象"
        );
        assert_eq!(
            manager.get_message(
                "report_raw_frame",
                Some(&args(&[
                    ("index", 1usize.into()),
                    ("frame", "{\"choices\":[]}".into())
                ]))
            ),
            "  原始片段 1: {\"choices\":[]}"
        );
        // 这两个空格在**续行**上：Fluent 剥掉续行的缩进后，值自己带的 `{"  "}`
        // 才是那两格——它不在 grep 的首行命中里，靠形状扫描才看得见。
        assert_eq!(
            manager.get_message(
                "no_content_error",
                Some(&args(&[(
                    "body",
                    "生成 commit 消息失败：模型没有返回任何内容".into()
                )]))
            ),
            "生成 commit 消息失败：模型没有返回任何内容\n  提示: 加 --debug 查看完整原始响应；或在 config.json 中调整 max_tokens"
        );
        // 下面三条同样是「`⚠️`/`✍️` 后面两个空格」，但它们既不在上面那组，
        // 也不在 main.rs/commit.rs 里：`truncated_output_warning` 属于 ai.rs，
        // `config_auto_commit` 与 `final_message` 在 main.rs。上一轮的边界注释
        // 只点名了 `report_*` 和 `no_content_error`，漏掉了 `truncated_output_warning`。
        assert_eq!(
            manager.get_message(
                "truncated_output_warning",
                Some(&args(&[
                    ("what", "生成 commit 消息".into()),
                    ("limit", 64u32.into())
                ]))
            ),
            "⚠️  生成 commit 消息的模型输出在 max_tokens=64 处被截断，内容可能不完整"
        );
        assert_eq!(
            manager.get_message("config_auto_commit", None),
            "⚙️  config.json 中 auto_commit=true，跳过确认"
        );
        assert_eq!(
            manager.get_message("final_message", None),
            "✍️  正在生成最终提交消息..."
        );

        // 反向属性：下面这三个提示语的值**首尾都不能有空格**。用户在提示语后面看到的
        // 那个空格是调用处补的（main.rs:22 的 `print!("{} ", prompt)`）；值里再写一个
        // `{" "}`，终端上就变成两个空格——静默，而且**不是只有中文会中招**：
        // en-US.ftl:12,14,16 那三个值走的是同一个 `print!("{} ", prompt)`，
        // 在英文值里加一个空格是一样的翻倍效果。这句以前写成「只有中文这一格会中招」，
        // 那是把「本守卫只读 zh-CN」说成了「只有 zh-CN 有问题」。
        // 钉的依旧是原字面量本身：原字面量的结尾没有空格。
        assert_eq!(
            manager.get_message("model_input_prompt", None),
            "请输入模型名称 (e.g., deepseek-chat):"
        );
        assert_eq!(
            manager.get_message("base_url_input_prompt", None),
            "请输入模型 API URL:"
        );
        assert_eq!(
            manager.get_message("api_token_input_prompt", None),
            "请输入 API Token:"
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
