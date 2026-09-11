//! 诊断输出开关与脱敏工具。
//!
//! 开关用进程级 `AtomicBool` 而不是给每个函数加参数：调用链很深，
//! 逐层传参会把签名污染得到处都是。**所有输出都走 stderr** ——
//! stdout 正在流式打印即将提交的 commit 消息，混进去会污染它。

use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// 单帧最多打印多少字符
pub const MAX_FRAME_CHARS: usize = 2000;
/// 错误信息里保留多少条原始帧
pub const RAW_TAIL_FRAMES: usize = 3;
/// 错误信息里每条原始帧保留多少字符
pub const RAW_TAIL_CHARS: usize = 200;
/// 非 2xx 响应体最多带多少字符进错误信息
pub const MAX_ERROR_BODY_CHARS: usize = 500;

/// 截断长文本，并注明原始长度。
pub fn truncate(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars).collect();
    format!("{}…(截断，原文 {} 字)", head, total)
}

/// 把已知密钥从文本里抹掉。
///
/// 这是防泄漏的**兜底**：即便网关把 Authorization 头回显进响应帧或错误体，
/// 密钥字面量也不会出现在任何输出里。
pub fn redact_secrets(text: &str, secrets: &[&str]) -> String {
    let mut out = text.to_string();
    for secret in secrets {
        // 太短的串做替换会误伤正常文本
        if secret.len() >= 8 {
            out = out.replace(secret, "<已隐藏>");
        }
    }
    out
}

/// 响应头名里出现这些词就隐藏其值。
pub fn is_sensitive_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["auth", "token", "key", "cookie", "secret"]
        .iter()
        .any(|needle| name.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_short_text_intact() {
        assert_eq!(truncate("短文本", 10), "短文本");
    }

    #[test]
    fn truncate_reports_original_length() {
        let out = truncate("一二三四五", 3);

        assert!(out.starts_with("一二三"));
        assert!(out.contains("原文 5 字"), "{out}");
    }

    /// 按字符截断，不能把多字节字符切成两半
    #[test]
    fn truncate_is_char_safe() {
        let out = truncate(&"字".repeat(100), 5);

        assert!(out.starts_with("字字字字字"));
        assert!(!out.contains('\u{FFFD}'));
    }

    #[test]
    fn redact_secrets_removes_token() {
        let out = redact_secrets(
            "Authorization: Bearer sk-abcdefghijklmn",
            &["sk-abcdefghijklmn"],
        );

        assert!(!out.contains("sk-abcdefghijklmn"));
        assert!(out.contains("<已隐藏>"));
    }

    /// 过短的密钥不做替换，避免误伤正常文本
    #[test]
    fn redact_secrets_ignores_short_secret() {
        assert_eq!(
            redact_secrets("a normal sentence", &["a"]),
            "a normal sentence"
        );
    }

    #[test]
    fn detects_sensitive_header_names() {
        assert!(is_sensitive_header("Authorization"));
        assert!(is_sensitive_header("X-Api-Key"));
        assert!(is_sensitive_header("Set-Cookie"));
        assert!(!is_sensitive_header("Content-Type"));
    }
}
