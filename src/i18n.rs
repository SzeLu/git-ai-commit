// src/i18n.rs
//!
//! 轻量 i18n：从 `locales/*.ftl` 读取 Fluent 资源，按「精确 locale → 主语言 → en-US」
//! 逐级回退（spec §5.1）。整条链都取不到时返回 key 本身——这样即使翻译缺失，
//! 界面也只会露出 key，而不会 panic 或输出空白。

use fluent::{FluentBundle, FluentResource, FluentValue};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use unic_langid::LanguageIdentifier;

/// 回退链的最后一格。
const FALLBACK_LOCALE: &str = "en-US";

/// 资源目录。这里按当前工作目录解析，与 CLI 从仓库根运行的既有约定一致；
/// 不做资源内嵌或路径探测——那不在本计划范围内。
const LOCALES_DIR: &str = "locales";

/// i18n 初始化失败的原因。
///
/// 注意 `locales/` 目录或某个具体 `.ftl` 不存在**不算失败**：那只说明这一级
/// 回退没有内容，链路继续往下走（见 [`I18nManager::init`]）。
#[derive(Debug)]
pub enum I18nError {
    /// 语言标签不是合法的 BCP 47（例如旧配置里写的是 `Chinese` 这种自然语言名）
    InvalidLocale(String),
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
            Self::InvalidLocale(locale) => {
                write!(
                    f,
                    "无法解析语言标签 \"{locale}\"（应为 BCP 47，例如 zh-CN）"
                )
            }
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
    pub fn init(config_language: Option<String>) -> Result<Self, I18nError> {
        let requested = config_language
            .filter(|language| !language.trim().is_empty())
            .or_else(sys_locale::get_locale)
            .unwrap_or_else(|| FALLBACK_LOCALE.to_string());
        let requested = requested.trim();

        let locale: LanguageIdentifier = requested
            .parse()
            .map_err(|_| I18nError::InvalidLocale(requested.to_string()))?;

        let mut bundles = Vec::new();
        for candidate in fallback_chain(&locale) {
            if let Some(bundle) = load_bundle(&candidate)? {
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

/// 回退链：精确 locale → 主语言 → en-US，按序去重（`en-US` 自己只需查一遍）。
fn fallback_chain(locale: &LanguageIdentifier) -> Vec<String> {
    let mut chain = vec![locale.to_string()];

    let language = locale.language.to_string();
    if language != chain[0] {
        chain.push(language);
    }
    if !chain.iter().any(|candidate| candidate == FALLBACK_LOCALE) {
        chain.push(FALLBACK_LOCALE.to_string());
    }

    chain
}

/// 加载一个 BCP 47 标签对应的 `.ftl`。
///
/// 文件不存在返回 `Ok(None)`——这一级回退空缺，让链路继续往下走。
fn load_bundle(locale: &str) -> Result<Option<FluentBundle<FluentResource>>, I18nError> {
    let path = Path::new(LOCALES_DIR).join(format!("{locale}.ftl"));
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

    let langid: LanguageIdentifier = locale
        .parse()
        .map_err(|_| I18nError::InvalidLocale(locale.to_string()))?;

    let mut bundle = FluentBundle::new(vec![langid]);
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

    #[test]
    fn test_missing_key() {
        let manager = I18nManager::init(Some("en-US".to_string())).unwrap();
        let msg = manager.get_message("non_existent_key", None);
        assert_eq!(msg, "non_existent_key");
    }
}
