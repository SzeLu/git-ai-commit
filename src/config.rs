// src/config.rs
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    /// 默认使用的模型名称（兼容旧配置）
    #[serde(default = "default_model")]
    pub model: String,
    /// 允许配置多个大模型的列表（按优先级顺序）
    #[serde(default = "default_models")]
    pub models: HashMap<String, ModelConfig>,
    /// 当前选中的模型，默认使用 `model`
    #[serde(default = "default_selected_model")]
    pub selected_model: String,
    /// 最终生成 commit 消息时的 token 上限
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    /// 是否跳过提交确认（与命令行 `--auto` 等价，`--dry-run` 优先）
    #[serde(default = "default_auto_commit")]
    pub auto_commit: bool,
    /// 是否在提交前强制校验 Conventional Commits 格式
    #[serde(default = "default_strict_format")]
    pub strict_format: bool,
    /// 摘要调用的 token 上限；缺省时继承 `max_tokens`
    #[serde(default)]
    pub summary_max_tokens: Option<u32>,
    /// 摘要调用的温度；缺省时继承 `temperature`
    #[serde(default)]
    pub summary_temperature: Option<f32>,
    /// diff 超过多少字节才启用分块，默认 8000
    #[serde(default = "default_chunk_threshold")]
    pub chunk_threshold: usize,
}

/// 一次生成请求的参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GenParams {
    pub max_tokens: u32,
    pub temperature: f32,
}

fn default_model() -> String {
    "".to_string()
}

fn default_max_tokens() -> u32 {
    32768
}

fn default_temperature() -> f32 {
    0.7
}

fn default_auto_commit() -> bool {
    false
}

fn default_strict_format() -> bool {
    true
}

fn default_chunk_threshold() -> usize {
    8000
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ModelConfig {
    pub model: String,
    pub base_url: String,
    pub api_token: String,
}

fn default_models() -> HashMap<String, ModelConfig> {
    HashMap::new()
}

fn default_selected_model() -> String {
    "".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: "".to_string(),
            models: HashMap::new(),
            selected_model: "".to_string(),
            max_tokens: default_max_tokens(),
            temperature: default_temperature(),
            auto_commit: default_auto_commit(),
            strict_format: default_strict_format(),
            summary_max_tokens: None,
            summary_temperature: None,
            chunk_threshold: default_chunk_threshold(),
        }
    }
}

impl Config {
    /// 读取配置文件；文件不存在或无法解析时返回 `None`。
    ///
    /// 这里不再顺手写回一份默认配置：空的默认配置没有任何可用模型，
    /// 写出去既没意义，又会把“还没初始化”这个状态掩盖成“已配置”。
    /// 首次运行由调用方交互式补齐后再 `save`。
    pub fn load() -> Option<Self> {
        let config_path = Self::get_config_path().ok()?;
        let content = std::fs::read_to_string(config_path).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// 当前生效的模型配置。
    ///
    /// 依次尝试 `selected_model` 和兼容旧配置的 `model` 字段；
    /// 都取不到时返回 `None`，表示调用方需要交互式初始化。
    pub fn active_model(&self) -> Option<&ModelConfig> {
        [self.selected_model.as_str(), self.model.as_str()]
            .into_iter()
            .filter(|name| !name.is_empty())
            .find_map(|name| self.models.get(name))
    }

    /// 摘要请求的参数。
    ///
    /// 缺省继承 `max_tokens` / `temperature`，保证「调参走 config.json」这条规则
    /// 对摘要同样成立；`summary_*` 只是给「全局上限很大、但想单独给摘要设界」的场景。
    pub fn summary_params(&self) -> GenParams {
        GenParams {
            max_tokens: self.summary_max_tokens.unwrap_or(self.max_tokens),
            temperature: self.summary_temperature.unwrap_or(self.temperature),
        }
    }

    /// 最终生成 commit 消息的参数。
    pub fn final_params(&self) -> GenParams {
        GenParams {
            max_tokens: self.max_tokens,
            temperature: self.temperature,
        }
    }

    pub fn save(&self) -> Result<()> {
        let config_path = Self::get_config_path()?;
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(config_path, content)?;
        Ok(())
    }

    fn get_config_path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("无法获取 home 目录")?;
        Ok(home.join(".git-ai-commit").join("config.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(selected: &str, legacy: &str, names: &[&str]) -> Config {
        let mut config = Config::default();
        for name in names {
            config.models.insert(
                (*name).to_string(),
                ModelConfig {
                    model: (*name).to_string(),
                    base_url: "http://localhost/v1".to_string(),
                    api_token: "token".to_string(),
                },
            );
        }
        config.selected_model = selected.to_string();
        config.model = legacy.to_string();
        config
    }

    #[test]
    fn active_model_prefers_selected_model() {
        let config = config_with("b", "a", &["a", "b"]);
        assert_eq!(config.active_model().unwrap().model, "b");
    }

    /// 旧配置只填了 `model` 字段时，应回退到它而不是判定为“未配置”。
    #[test]
    fn active_model_falls_back_to_legacy_field() {
        let config = config_with("", "a", &["a"]);
        assert_eq!(config.active_model().unwrap().model, "a");
    }

    #[test]
    fn active_model_is_none_when_unusable() {
        // 空配置（首次运行）
        assert!(Config::default().active_model().is_none());
        // selected_model 指向一个不存在的条目
        assert!(config_with("missing", "", &["a"]).active_model().is_none());
    }

    /// 用 serde_json 直接解析，而不是 `Config::load()`——后者读 `$HOME`，
    /// 在并行单测里是进程级竞态。
    fn parse(json: &str) -> Config {
        serde_json::from_str(json).expect("配置应能解析")
    }

    /// 回归测试：缺少可选项的旧 config.json 必须仍能解析。
    /// 这些字段以前是必填，缺一个就会让整个文件解析失败并触发交互式重录。
    #[test]
    fn parses_legacy_config_missing_optional_keys() {
        let config = parse(
            r#"{
                "models": {"m": {"model": "m", "base_url": "http://x/v1", "api_token": "t"}},
                "selected_model": "m"
            }"#,
        );

        assert_eq!(config.max_tokens, 32768);
        assert_eq!(config.temperature, 0.7);
        assert!(!config.auto_commit);
        assert!(config.strict_format);
        assert_eq!(config.chunk_threshold, 8000);
        assert!(config.active_model().is_some());
    }

    /// 一个只写了模型信息的极简配置也要能用
    #[test]
    fn parses_minimal_config() {
        let config = parse(
            r#"{"models": {"m": {"model": "m", "base_url": "http://x/v1", "api_token": "t"}},
                "selected_model": "m", "max_tokens": 1234}"#,
        );

        assert_eq!(config.max_tokens, 1234);
    }

    /// 摘要缺省继承 max_tokens / temperature
    #[test]
    fn summary_params_inherit_from_global() {
        let config = parse(
            r#"{"models": {}, "selected_model": "m", "max_tokens": 4096, "temperature": 0.4}"#,
        );

        assert_eq!(config.summary_params().max_tokens, 4096);
        assert_eq!(config.summary_params().temperature, 0.4);
    }

    /// 显式配置摘要参数时覆盖继承值
    #[test]
    fn summary_params_can_be_overridden() {
        let config = parse(
            r#"{"models": {}, "selected_model": "m", "max_tokens": 4096, "temperature": 0.4,
                "summary_max_tokens": 512, "summary_temperature": 0.1}"#,
        );

        assert_eq!(config.summary_params().max_tokens, 512);
        assert_eq!(config.summary_params().temperature, 0.1);
        // 最终生成不受摘要配置影响
        assert_eq!(config.final_params().max_tokens, 4096);
        assert_eq!(config.final_params().temperature, 0.4);
    }

    #[test]
    fn chunk_threshold_is_configurable() {
        let config = parse(r#"{"models": {}, "selected_model": "m", "chunk_threshold": 100000}"#);

        assert_eq!(config.chunk_threshold, 100000);
    }
}
