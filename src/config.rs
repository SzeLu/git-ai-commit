// src/config.rs
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    /// 默认使用的模型名称（兼容旧配置）
    #[serde(default = "default_model")]
    pub model: String,
    /// 允许配置多个大模型的列表（按优先级顺序）
    #[serde(default = "default_models")]
    pub models: Vec<String>,
    /// 当前选中的模型，默认使用 `model`
    #[serde(default = "default_selected_model")]
    pub selected_model: String,
    pub max_tokens: u16,
    pub temperature: f32,
    pub auto_commit: bool,
    pub strict_format: bool,
}

fn default_model() -> String {
    "deepseek-chat".to_string()
}
fn default_models() -> Vec<String> {
    vec!["deepseek-chat".to_string()]
}
fn default_selected_model() -> String {
    "deepseek-chat".to_string()
}


impl Default for Config {
    fn default() -> Self {
        Self {
            model: "deepseek-chat".to_string(),
            models: vec!["deepseek-chat".to_string()],
            selected_model: "deepseek-chat".to_string(),
            max_tokens: 1000,
            temperature: 0.7,
            auto_commit: false,
            strict_format: true,
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = Self::get_config_path()?;
        if config_path.exists() {
            let content = std::fs::read_to_string(config_path)?;
            let config: Config = serde_json::from_str(&content)?;
            Ok(config)
        } else {
            let config = Config::default();
            config.save()?;
            Ok(config)
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
