// src/i18n.rs
//!
//! 轻量 i18n：从**内嵌**进二进制的 Fluent 资源（`locales/*.ftl`，见 `LOCALE_SOURCES`）
//! 里取文案，按「精确 locale → 主语言 → en-US」逐级回退（spec §5.1）。
//! 整条链都取不到时返回 key 本身——这样即使翻译缺失，界面也只会露出 key，
//! 而不会 panic 或输出空白。

use crate::debug;
// 用 `concurrent::FluentBundle`（memoizer 内部是 Mutex）而不是默认的
// `FluentBundle`（内部是 RefCell）：只有前者是 `Send + Sync`，而下面的进程级
// `OnceLock<I18nManager>` 要求它。默认那个连 `static` 都放不进去——这不是
// 「换个写法」的偏好，是编译期硬性条件。两者的格式化结果完全一致。
use fluent::concurrent::FluentBundle;
use fluent::{FluentResource, FluentValue};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::OnceLock;
use unic_langid::{langid, LanguageIdentifier};

/// 回退链的最后一格。
const FALLBACK_LOCALE: &str = "en-US";

/// 内嵌的语言资源：文件名词干 → `.ftl` 源文本（spec §6「或打包进二进制分发」）。
///
/// 以前这里是个 `LOCALES_DIR = "locales"` 常量，靠 `Path::new(LOCALES_DIR)` 在运行期
/// 读盘——那是**相对当前工作目录**解析的。而 `install.sh` 只把可执行文件拷进
/// `~/.local/bin`，工具又是借全局别名 `git aic` 在**用户自己的仓库**里跑的：那一刻
/// 的工作目录是用户的仓库，`./locales/` 根本不是我们的目录，回退链每一级都落空，
/// 界面上只剩裸 key（`git aic` 打出 `not_git_repo` 而不是 `❌ 当前目录不是 Git 仓库`）。
/// 测试看不见这个缺陷，因为 `cargo test` 的工作目录恰好是仓库根，`locales/` 就在那儿。
/// 内嵌之后资源的来源与运行位置无关，这一整类故障才算了结。
///
/// 只做内嵌，**不做**「先读盘、读不到再退回内嵌」：很多用户仓库自带 `locales/` 目录，
/// 磁盘优先会让用户自己的同名文件（比如他的 `locales/zh-CN.ftl`）盖掉我们的资源，
/// 中文静默失效——那比「找不到」难查得多。
///
/// 顺序无关；但新增 `.ftl` **必须**同步加进这张表，漏了就只是二进制里没有那个文件
/// （`load_bundle` 把它当成「这一级没有资源」），不会有任何编译或运行期报错。
const LOCALE_SOURCES: &[(&str, &str)] = &[
    ("zh-CN", include_str!("../locales/zh-CN.ftl")),
    ("en-US", include_str!("../locales/en-US.ftl")),
    ("fr", include_str!("../locales/fr.ftl")),
    ("fr-US", include_str!("../locales/fr-US.ftl")),
];

/// 按文件名词干取内嵌的源文本；表里没有这一级就是 `None`。
fn locale_source(stem: &str) -> Option<&'static str> {
    LOCALE_SOURCES
        .iter()
        .find(|(name, _)| *name == stem)
        .map(|(_, source)| *source)
}

/// i18n 初始化失败的原因。
///
/// 只有「链的底板那一格坏了」才算失败。以下情况都**不算失败**：某个 `.ftl`
/// 没进 `LOCALE_SOURCES`（这一级回退没内容，链路继续往下走）、某一格语法不合法
/// （跳过它，链路继续往下走，见 [`I18nManager::init`]）、以及语言标签解析不了
/// （退回 en-US）。三种情况都不该让一个命令行工具拒绝启动。
///
/// 内嵌之后不再有「文件存在但读不出来」这一格：内容在编译期就进了二进制，
/// 运行期没有任何读盘动作，能坏的只剩语法——而那意味着我们自己发布了一份坏资源，
/// 属于构建期就该被发现的问题。
#[derive(Debug)]
pub enum I18nError {
    /// 内嵌的 `.ftl` 语法不合法。`path` 是这份内容的检出源头，仅用于报错定位。
    FtlParse { path: PathBuf, errors: Vec<String> },
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
        }
    }
}

impl std::error::Error for I18nError {}

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
    ///
    /// 某一格**坏了**（内嵌的 `.ftl` 语法不合法）只跳过这一格，链路继续往下走：
    /// 整条链一起丢掉的后果是连 en-US 底板都没了，用户会看到满屏裸 key，
    /// 比少一级回退严重得多。只有底板自己坏掉才是真的失败——那是链的最后一格，
    /// 它没了就无处可落。这一段的取舍见 [`load_chain`]。
    pub fn init(config_language: Option<String>) -> Result<Self, I18nError> {
        let requested = requested_locale(config_language, sys_locale::get_locale());

        let locale = negotiate_locale(&resolve_locale(&requested));

        let bundles = load_chain(fallback_chain(&locale), load_bundle)?;

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

/// 把历史配置里接受过的「自然语言名」规范成 BCP 47 标签。
///
/// 早期版本允许用户在 config.json 里写 `"Chinese"` / `"Japanese"` 这样的自然
/// 语言名（`src/config.rs` 的测试至今还把它们当合法输入钉着）。它们**不是**解析
/// 失败的那一类——`"Chinese"` 按 7 个字母能塞进 BCP 47 的 language 子标签位，
/// `parse` 会把它解析成一个语言为 `chinese` 的合法标签，只是没有对应的 `.ftl`。
/// 于是界面整条链落空变成英文，而 prompt 模板里却带着原样值，出现「英文界面 +
/// 中文提交消息」这种错位。所以这里必须先于解析把它们翻译成标签，后面的一切
/// （解析、协商、回退链）才都走同一条路。
///
/// 返回 `None` 表示「不是已知的自然语言名」，调用方按普通标签继续解析。
fn normalize_language_name(input: &str) -> Option<&'static str> {
    let trimmed = input.trim();
    // 英文名不区分大小写（旧配置可能写 "Chinese" 或 "chinese"）；中文名没有
    // 大小写之分，直接按原文比。两段匹配都只覆盖「历史接受过」的那几个值，
    // 不替用户猜更多。
    let lower = trimmed.to_ascii_lowercase();
    match lower.as_str() {
        "chinese" => Some("zh-CN"),
        "english" => Some("en-US"),
        "japanese" => Some("ja"),
        _ => match trimmed {
            "中文" | "简体中文" => Some("zh-CN"),
            "英文" => Some("en-US"),
            "日本語" => Some("ja"),
            _ => None,
        },
    }
}

/// 把配置或系统给出的语言串解析成 locale。
///
/// 先过一遍 [`normalize_language_name`]，把自然语言名翻译成 BCP 47，其余
/// 字符串按标签原样解析。解析不了就退回 en-US（spec §4 那条链的最后一格）。
/// **配置值走这里，系统检测值也走这里**——一个命令行工具不该因为读不懂一个
/// 语言串就拒绝运行。
fn resolve_locale(requested: &str) -> LanguageIdentifier {
    normalize_language_name(requested)
        .unwrap_or_else(|| requested.trim())
        .parse()
        .unwrap_or_else(|_| langid!("en-US"))
}

/// 把解析出来的 locale 与内嵌资源里**实际存在的词干**协商。
///
/// 系统的语言检测会给带脚本的标签：macOS 的 `CFLocaleCopyPreferredLanguages`
/// 对中文用户返回 `zh-Hans-CN`。而内嵌资源只按 `zh-CN` 起名，`fallback_chain`
/// 只生成 `zh-Hans-CN` / `zh` / `en-US` 三格——前两格都没有文件，整条链
/// 直接落到英文，中文用户拿到的是一整个英文界面。这里用 `matches` 找出能
/// **覆盖**请求语言的内嵌词干：词干当「范围」，缺省的子标签是通配符，所以
/// `zh-CN`（无脚本）能覆盖 `zh-Hans-CN`（带脚本），而 `zh-Hant-TW` 因为地区
/// 不同覆盖不了 `zh-CN`、`en-GB` 也覆盖不了 `en-US`。
///
/// 精确命中优先：请求的就是某个内嵌词干时原样保留——否则 `fr-US` 会被更宽松的
/// `fr` 吞掉，丢掉它自己的文件。协商不中则原样返回，交给 [`fallback_chain`]
/// 走老逻辑回退——没映射上的 locale 的表现与改动前完全一致，不能因此变差。
fn negotiate_locale(requested: &LanguageIdentifier) -> LanguageIdentifier {
    let requested_stem = requested.to_string();
    if locale_source(&requested_stem).is_some() {
        return requested.clone();
    }
    for (stem, _) in LOCALE_SOURCES {
        // LOCALE_SOURCES 里的词干都是合法 BCP 47（见上方常量表注释），unwrap 不会失败。
        let Ok(stem_locale) = stem.parse::<LanguageIdentifier>() else {
            continue;
        };
        if stem_locale.matches(requested, true, false) {
            return stem_locale;
        }
    }
    requested.clone()
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

/// 逐级加载整条回退链，把「单格坏了怎么办」这一个决策收在一处。
///
/// 单格坏掉（内嵌的 `.ftl` 语法不合法）只是**跳过**这一格，链路继续往下走：丢掉
/// 整条链等于连 en-US 底板一起丢，用户看到的是满屏裸 key，比少一级回退严重得多。
/// 只有底板（`FALLBACK_LOCALE`）自己坏掉才返回 `Err`——它是链的最后一格，没有
/// 「再往下」可言。
///
/// 收成一个函数、把「怎么加载一格」做成参数，是为了让上面这条决策可被测试：
/// 内嵌之后坏资源只能来自一份写坏的 `.ftl`，而那种文件在测试里造不出来
/// （测试不能改 `locales/`），不把 loader 抽出来，这个分支就只能靠读代码相信。
fn load_chain<F>(
    chain: Vec<(String, LanguageIdentifier)>,
    loader: F,
) -> Result<Vec<FluentBundle<FluentResource>>, I18nError>
where
    F: Fn(&str, &LanguageIdentifier) -> Result<Option<FluentBundle<FluentResource>>, I18nError>,
{
    let mut bundles = Vec::new();
    for (stem, bundle_locale) in chain {
        match loader(&stem, &bundle_locale) {
            Ok(Some(bundle)) => bundles.push(bundle),
            // 这一级没有资源：空缺，往下走。
            Ok(None) => {}
            Err(err) => {
                if stem == FALLBACK_LOCALE {
                    return Err(err);
                }
                // 跳过的这一格不阻断启动，但也不该无声无息：--debug 下留一句原因，
                // 免得「某一级悄悄没生效」变成只能靠肉眼比对界面才发现的事。
                // （不新增 I18nError 变体去携带逐级细节——spec §7.1 定了形状。）
                if debug::enabled() {
                    eprintln!("[debug] i18n: 跳过坏掉的语言资源 {stem}.ftl: {err}");
                }
            }
        }
    }

    Ok(bundles)
}

/// 加载一个文件名词干对应的 `.ftl`。
///
/// 资源是内嵌的，所以这里只有两种结果：表里没有这个文件名词干（`Ok(None)`，
/// 表示这一级回退空缺，链路继续往下走），或者内嵌的源文本解析不了（`Err`，
/// 由 [`load_chain`] 决定是跳过还是失败）。
fn load_bundle(
    stem: &str,
    locale: &LanguageIdentifier,
) -> Result<Option<FluentBundle<FluentResource>>, I18nError> {
    let Some(source) = locale_source(stem) else {
        return Ok(None);
    };

    // 报错时依旧要指出是哪个文件：内嵌之后没有真实路径可指，但 `locales/<stem>.ftl`
    // 就是这份内容的检出源头，要改的是它。
    let path = PathBuf::from(format!("locales/{stem}.ftl"));

    // FluentResource 收的是 String（源文本要活到资源里），内嵌的是 &'static str，
    // 于是这里有一次拷贝——每个 `.ftl` 每次 init 一次，只在进程启动时发生。
    let resource =
        FluentResource::try_new(source.to_string()).map_err(|(_, errors)| I18nError::FtlParse {
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

    /// 回归测试（本轮修复的核心）：macOS 上 `sys_locale` 给出的带脚本标签
    /// `zh-Hans-CN` 必须落到内嵌的 `zh-CN` 资源，渲染出中文。
    ///
    /// 只断言 `init` 返回 `Ok` 远远不够——修复前 `init(Some("zh-Hans-CN"))` 也是
    /// Ok，只是回退链（`zh-Hans-CN` / `zh` / `en-US`）前两格都没有文件，静默落到
    /// 英文，中文用户拿到的是一整个英文界面，而且 `zh-Hans-CN` 还会被写回
    /// config.json 持久化。必须钉住**真实键渲染出的中文原文**：这条断言正是前五轮
    /// 逐个任务评审都看不见这个缺陷的原因，也是它在本轮必须补上的唯一一道防线。
    #[test]
    fn zh_hans_cn_renders_chinese() {
        let manager = I18nManager::init(Some("zh-Hans-CN".to_string()))
            .expect("zh-Hans-CN 协商失败不应让 init 失败");
        assert_eq!(
            manager.get_message("not_git_repo", None),
            "❌ 当前目录不是 Git 仓库"
        );
    }

    /// 一格的坏资源只丢自己，不该丢掉整条链；只有底板坏掉才是真的失败。
    ///
    /// 内嵌之后「坏资源」只能来自一份写坏的 `.ftl`，而测试不许改 `locales/`，
    /// 所以这条借 `load_chain` 的 loader 参数来造：给 `zh-CN` 那一格喂一个解析错误，
    /// 结果里必须仍然有 en-US 底板——界面走英文，而不是满屏裸 key。
    /// （断言英文而不是「非空」是有意的：如果跳过的格子仍然进了 bundles，
    /// 这里拿到的会是中文。）
    #[test]
    fn a_broken_rung_is_skipped_and_only_a_broken_base_is_fatal() {
        let broken = |stem: &str| I18nError::FtlParse {
            path: PathBuf::from(format!("locales/{stem}.ftl")),
            errors: vec!["写坏的资源".to_string()],
        };

        let manager = I18nManager {
            bundles: load_chain(fallback_chain(&langid!("zh-CN")), |stem, locale| {
                if stem == "zh-CN" {
                    return Err(broken(stem));
                }
                load_bundle(stem, locale)
            })
            .expect("非基底那一格坏掉不该让整条链失败"),
        };
        assert_eq!(
            manager.get_message("not_git_repo", None),
            "❌ The current directory is not a Git repository"
        );

        // 底板自己坏掉：它是链的最后一格，没有「再往下」可言，这才是真的失败。
        let result = load_chain(fallback_chain(&langid!("zh-CN")), |stem, locale| {
            if stem == FALLBACK_LOCALE {
                return Err(broken(stem));
            }
            load_bundle(stem, locale)
        });
        assert!(
            result.is_err(),
            "en-US 底板坏掉必须返回 Err，否则界面只会剩下 key"
        );
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

    /// 历史配置接受过的自然语言名必须规范成 BCP 47，再走后续的协商与回退链。
    ///
    /// 这些值不是「解析失败」——`"Chinese"` 会被当成 7 个字母的语言子标签解析成
    /// `chinese`，只是没有对应 `.ftl`，于是界面英文、prompt 里却带着原样值。
    /// 端到端断言钉住「`init(Some("Chinese"))` 渲染中文」这一整条路径。
    #[test]
    fn legacy_language_names_normalize_to_bcp47() {
        assert_eq!(resolve_locale("Chinese").to_string(), "zh-CN");
        assert_eq!(resolve_locale("chinese").to_string(), "zh-CN");
        assert_eq!(resolve_locale("中文").to_string(), "zh-CN");
        assert_eq!(resolve_locale("简体中文").to_string(), "zh-CN");
        assert_eq!(resolve_locale("English").to_string(), "en-US");
        assert_eq!(resolve_locale("英文").to_string(), "en-US");
        assert_eq!(resolve_locale("Japanese").to_string(), "ja");
        assert_eq!(resolve_locale("日本語").to_string(), "ja");

        let manager = I18nManager::init(Some("Chinese".to_string()))
            .expect("自然语言名规范化失败不应让 init 失败");
        assert_eq!(
            manager.get_message("not_git_repo", None),
            "❌ 当前目录不是 Git 仓库"
        );
    }

    /// 协商层依赖的 `matches` 语义，逐条钉死，外加协商层的落点。
    ///
    /// `unic_langid` 的 `matches` 把 `self` 当成带通配的「范围」：范围里缺省的
    /// 脚本/地区子标签是通配符。协商时内嵌词干当范围、请求标签当具体值，
    /// 即 `词干.matches(请求, true, false)`。这三条正是协商正确性的全部前提：
    ///
    /// - 脚本缺省不算约束：`zh-CN` 覆盖 `zh-Hans-CN`（语言、地区一致，脚本只在请求侧）；
    /// - `zh-CN` **不**覆盖 `zh-Hant-TW`：两标签地区不同（CN≠TW）——所以繁体不会被
    ///   静默塞给简体，落到 en-US 是预期内的；
    /// - `en-US` 不覆盖 `en-GB`：地区不同（US≠GB）。
    #[test]
    fn negotiate_matches_semantics() {
        let zh_cn = langid!("zh-CN");
        assert!(zh_cn.matches(&langid!("zh-Hans-CN"), true, false));
        assert!(!zh_cn.matches(&langid!("zh-Hant-TW"), true, false));
        assert!(!langid!("en-US").matches(&langid!("en-GB"), true, false));

        // 协商层的落点：能映射的映射，映射不了的原样返回（交给回退链，不得回归）。
        assert_eq!(negotiate_locale(&langid!("zh-Hans-CN")), langid!("zh-CN"));
        assert_eq!(
            negotiate_locale(&langid!("zh-Hant-TW")),
            langid!("zh-Hant-TW")
        );
        assert_eq!(negotiate_locale(&langid!("en-GB")), langid!("en-GB"));
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

    /// 所有对空白敏感的键，逐字节对齐原字面量；键集本身由派生保证完备。
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
    /// 正类的定义：**值的首或尾有空格，或含连续两个及以上空格**（`{"  "}` 这种占位符
    /// 按它代表的空格算）。定义写死在这里没用——「这一类共 N 个」这种完备性声明靠手写
    /// 维护，在本分支上已经错过两次（先是漏了整类，补完又漏了 `truncated_output_warning`），
    /// 两次都是测试全绿而覆盖面小于它自称的范围。所以覆盖面的**计算**交给
    /// `derived_whitespace_sensitive_keys`：从 `.ftl` 源文本现算一遍，再断言它与
    /// `WHITESPACE_SENSITIVE_PINS` **双向相等**——漏钉一个（源里新加了一个带双空格的
    /// 值）和多钉一个（钉了一个其实不敏感的键）都会让本测试红。下面逐条的
    /// `assert_eq!` 查的是**内容**（那几个空格的字节形状对不对），派生查的是**覆盖面**
    /// （该查的键一个不少），两件事互不替代。
    ///
    /// 因此本测试现在的完备性是可执行的，而不是自称的；`en-US.ftl` 的值仍然**没有**
    /// 被这样覆盖：本测试只读 zh-CN 这一格（`locales/en-US.ftl` 里同样有 `⚠️` 后两个
    /// 空格的值，多写一个 `{" "}` 会一样地把空格翻倍）。两个文件的键集由
    /// `zh_cn_keys_are_all_present_in_en_us` 守；要连 en-US 的空白形状一起钉，
    /// 得让本测试对两个 manager 各跑一遍——那件事没做，别以为做了。
    #[test]
    fn whitespace_sensitive_keys_match_their_original_literals() {
        let manager = I18nManager::init(Some("zh-CN".to_string())).unwrap();
        let args = |pairs: &[(&str, FluentValue<'static>)]| {
            pairs
                .iter()
                .map(|(name, value)| ((*name).to_string(), value.clone()))
                .collect::<HashMap<String, FluentValue<'static>>>()
        };
        // 派生集合要在下面几处断言里用到，且必须和逐条断言读同一份源文本。
        let derived = derived_whitespace_sensitive_keys(&read_locale("zh-CN"), &manager);

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

        // ── 覆盖面：派生集合 vs 钉子集合，双向断言 ──────────────────────────
        // 上面逐条的 assert_eq! 证明了「钉住的那些值确实是对的」；这一段证明
        // 「该钉的都在上面」。缺任何一边，这类回归都还能从缝里溜过去：
        // 只在源里新加一个带双空格的值（派生有、没钉）→ unpinned 非空；
        // 或把一个键从敏感改成不敏感、或钉了一个根本不属于正类的键
        // （钉了、派生说不是）→ overpinned 非空。
        let pinned: BTreeSet<String> = WHITESPACE_SENSITIVE_PINS
            .iter()
            .map(|key| (*key).to_string())
            .collect();
        let unpinned: Vec<&String> = derived.difference(&pinned).collect();
        let overpinned: Vec<&String> = pinned.difference(&derived).collect();
        assert!(
            unpinned.is_empty() && overpinned.is_empty(),
            "空白敏感键的钉子与派生结果不一致：\n  派生有、没钉（新增了敏感值却没钉住）: {unpinned:?}\n  钉了、派生说不是（钉子已过期或不属于正类）: {overpinned:?}"
        );

        // 另外两个被钉的键不属于空白敏感正类，它们是为**换行形状**钉的
        // （多行值里那个空行、`{ $body }` 在去缩进之后落在哪里）。显式断言它们
        // 不在派生集合里：不写的话，读者无法区分「派生正确地排除了它们」和
        // 「派生漏算了它们」——而本守卫存在的理由正是「漏算看不出来」。
        for key in NEWLINE_SHAPE_PINS {
            assert!(
                !derived.contains(*key),
                "{key} 的值首尾没有空格、也没有连续两个空格，不该出现在空白敏感集合里；\
                 它是为换行形状钉的，若它真的落在正类里，说明值被改成了另一种形状（或者派生坏了）"
            );
        }

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
        // 反向属性也要钉住**它是反向的**：这三条不属于正类这件事本身要断言出来。
        // 否则哪天有人给其中一个值补了个 `{" "}`，正类会多出这一条（unpinned 报红，
        // 还算能发现），但要是派生或谓词写反了、把整类都算漏，这里仍会静静通过——
        // 显式断言让「正类必须不含它们」这句话本身可执行。
        for key in NO_EDGE_SPACE_PROMPTS {
            assert!(
                !derived.contains(*key),
                "{key} 的值首尾必须没有空格，却被算进了空白敏感集合"
            );
        }
    }

    /// 派生器自身的自证：对着 Fluent 那两条最容易算错的规则各钉一个例子。
    ///
    /// 上面那条测试的结论完全建立在「派生集合算得对」上——如果派生把该敏感的值漏掉，
    /// 它就只是在和一个同样漏掉的手写列表互相印证。这里不依赖真实 `.ftl`，用一段
    /// 合成源文本把判据本身钉死：源里多出一个敏感值，派生就必须多出那个键。
    #[test]
    fn whitespace_derivation_follows_fluent_rules() {
        // 规则一：`{"  "}` 这类字面量占位符按它代表的空格数算（值被渲染出来才有形状），
        // 所以 `{"  "}x` 是敏感值，而 `x { $v } y` 不是（单个空格）。
        // 规则二：续行先按公共缩进（这里是 2）去缩进，`      third` 比公共缩进多出来的
        // 那 4 个空格**会留下**，于是这个多行值同样是敏感值。
        // 最后一条是反向的例子：值以变量结尾时，不能因为「变量渲染成空串」而凭空
        // 多出一个结尾空格——变量要喂不含空白的哨兵值。
        let source = concat!(
            "placeable_spaces = {\"  \"}x\n",
            "continuation_extra = first\n  second\n      third\n",
            "plain_single_space = x { $v } y\n",
            "variable_at_end = x { $v }\n",
        );

        let resource = FluentResource::try_new(source.to_string()).unwrap();
        let mut bundle = FluentBundle::new_concurrent(vec![langid!("zh-CN")]);
        bundle.set_use_isolating(false);
        bundle.add_resource(resource).unwrap();
        let manager = I18nManager {
            bundles: vec![bundle],
        };

        assert_eq!(
            derived_whitespace_sensitive_keys(source, &manager),
            BTreeSet::from([
                "continuation_extra".to_string(),
                "placeable_spaces".to_string()
            ])
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

    /// 读一个 locale 的 `.ftl` 源文本——**与二进制里内嵌的是同一份字节**。
    ///
    /// 以前这里读盘（`{LOCALES_DIR}/{locale}.ftl`）：那既可能校验到与发布内容不同的
    /// 东西（磁盘上被改过、或 CWD 根本不是仓库根），又会在 CWD 变化时直接 panic。
    /// 指向 `LOCALE_SOURCES` 之后，校验的就是真正会被加载的那份内容，
    /// 本测试因此不再依赖「测试是在仓库根跑的」这个前提。
    fn read_locale(locale: &str) -> String {
        locale_source(locale)
            .unwrap_or_else(|| panic!("LOCALE_SOURCES 里没有 {locale}，新增 .ftl 时要同步加进去"))
            .to_string()
    }

    /// `LOCALE_SOURCES` 必须与 `locales/` 目录一一对应。
    ///
    /// 漏登记一个 `.ftl` 不会有任何报错：那个文件只是没进二进制，`load_bundle` 会把
    /// 整格当成「这一级没有资源」——正是本任务要根除的那种静默失败（界面照常显示，
    /// 只是某种语言永远命不中）。所以这里直接对目录断言一次，让「记得同步」这件事
    /// 由测试来说，而不是由注释来说。
    ///
    /// 路径取 `CARGO_MANIFEST_DIR`（编译期常量）而不是相对路径：上面 `read_locale`
    /// 刚刚才甩掉「测试必须在仓库根跑」这个前提，不能从这里再加回来。这里读的只是
    /// **文件名**，校验内容仍然是内嵌那份。
    #[test]
    fn locale_sources_covers_every_ftl_file_on_disk() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");

        let on_disk: BTreeSet<String> = std::fs::read_dir(&dir)
            .unwrap_or_else(|err| panic!("读不到 {}: {err}", dir.display()))
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter_map(|name| name.strip_suffix(".ftl").map(str::to_string))
            .collect();

        let embedded: BTreeSet<String> = LOCALE_SOURCES
            .iter()
            .map(|(stem, _)| (*stem).to_string())
            .collect();

        assert_eq!(
            on_disk, embedded,
            "locales/ 与 LOCALE_SOURCES 不一致：磁盘上有、表里没有的文件在发布版里永远取不到"
        );
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

    /// 「对空白敏感」的定义：值的首或尾有空格，或含连续两个及以上空格。
    fn has_sensitive_whitespace(value: &str) -> bool {
        value.starts_with(' ') || value.ends_with(' ') || value.contains("  ")
    }

    /// 从 `.ftl` 源文本里收集所有变量名（`$name`）。
    ///
    /// 派生空白形状时要先把变量喂上一个**不含空白**的哨兵值，否则渲染结果里会混进
    /// 与这条消息的形状无关的东西：
    /// - 取不到值的变量，Fluent 会把 `{$name}` 原样写进结果（见 fluent-bundle 的
    ///   `VariableReference` 分支），那不是值的形状，还可能贴出一个假的双空格；
    /// - 喂空串同样不行：值以变量结尾时（`…大模型: { $model }`），空串会凭空造出
    ///   一个结尾空格。
    ///
    /// 所以扫描只负责「有哪些变量」，值一律给 `"x"`：不引入空白，也不漏掉变量。
    /// 这不是在解析 FTL，只是按 `$` 起头的标识符扫一遍名字。
    fn ftl_variables(source: &str) -> Vec<String> {
        let is_name_start = |b: u8| b.is_ascii_alphabetic() || b == b'_';
        let bytes = source.as_bytes();
        let mut names: Vec<String> = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'$' && i + 1 < bytes.len() && is_name_start(bytes[i + 1]) {
                let start = i + 1;
                let end = bytes[start..]
                    .iter()
                    .position(|b| !(b.is_ascii_alphanumeric() || *b == b'_'))
                    .map_or(bytes.len(), |offset| start + offset);
                let name = &source[start..end];
                if !names.iter().any(|known| known == name) {
                    names.push(name.to_string());
                }
                i = end;
            } else {
                i += 1;
            }
        }
        names
    }

    /// 对空白敏感的键的**钉子**：下面逐字节断言其值的那一批。
    ///
    /// 这个列表只声明「我钉了哪些」；「该钉哪些」由 `derived_whitespace_sensitive_keys`
    /// 从 `.ftl` 源文本现算，测试断言两者相等。手写的完备性声明在本分支上已经错过两次
    /// （`truncated_output_warning` 漏过一轮），所以这里不再自称完备——记在下面 28 条
    /// 里的每一条都在正类的定义之内，反过来正类里的每一条也都在这里。
    const WHITESPACE_SENSITIVE_PINS: &[&str] = &[
        "block_details",
        "chunk_complete",
        "chunk_degraded",
        "chunk_progress",
        "config_auto_commit",
        "debug_hint",
        "degradation_warning",
        "degraded_confirmation_required",
        "edit_message_prompt",
        "final_generation_failed",
        "final_message",
        "format_validation_failed",
        "format_warning_nonstrict",
        "format_warning_strict",
        "long_diff_warning",
        "no_content_error",
        "report_content_frames",
        "report_diagnosis",
        "report_endpoint",
        "report_finish_reason",
        "report_frames",
        "report_raw_frame",
        "report_request",
        "report_stream_error",
        "strict_format_blocked_hint",
        "truncated_output_warning",
        "use_all_flag_hint",
        "validation_issues",
    ];

    /// 为**换行形状**而钉、但不属于空白敏感正类的两个键。
    ///
    /// 它们的值首尾没有空格、也没有连续两个空格，所以派生集合正确地不含它们；
    /// 它们被钉住是因为另一件事——多行值里那个空行、以及续行去缩进之后 `{ $body }`
    /// 落在哪里。两种属性混在一起时，「不在派生集合里」这件事必须写出来，
    /// 否则读者分不清它是被漏算了还是本来就不该算。
    const NEWLINE_SHAPE_PINS: &[&str] = &["fallback_more_files", "fallback_message"];

    /// 反向属性的三个键：值的首尾**不能**有空格。
    ///
    /// 与正类是不同的谓词——正类是「必须有空格」，这里是「必须没有」，所以要显式钉住，
    /// 不能靠「派生集合恰好没算进来」代替（那样一旦派生写反了，这里也静默通过）。
    const NO_EDGE_SPACE_PROMPTS: &[&str] = &[
        "model_input_prompt",
        "base_url_input_prompt",
        "api_token_input_prompt",
    ];

    /// 从 `.ftl` 源文本**派生**出对空白敏感的键集。
    ///
    /// 为什么让 Fluent 自己渲染一遍、而不是在源文本上正则扫：这条判据里有两个问题
    /// 只有解析器答得上来——`{"   "}` 这类字面量占位符要按它代表的空格数算；
    /// 多行值的续行会先按公共缩进做一次去缩进，**而比公共缩进多出来的那部分会留下**。
    /// 自己照着重写一遍这两条规则，写错的表现恰好是「少算了几个键」——也就是本守卫
    /// 存在的理由本身。渲染一次等于把解析器的答案直接拿来用，规则不会走样。
    fn derived_whitespace_sensitive_keys(source: &str, manager: &I18nManager) -> BTreeSet<String> {
        let args: HashMap<String, FluentValue<'static>> = ftl_variables(source)
            .into_iter()
            .map(|name| (name, FluentValue::from("x")))
            .collect();

        ftl_keys(source)
            .into_iter()
            .filter(|key| has_sensitive_whitespace(&manager.get_message(key, Some(&args))))
            .collect()
    }
}
