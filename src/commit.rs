use anyhow::Result;
use colored::*;
use regex::Regex;
use std::io::{self, Write};

/// 格式校验结果。
///
/// 校验与打印分开：提交前需要**静默**判断是否合规（`strict_format`），
/// 而 dry-run 需要把问题展示出来。原来校验函数自己 eprintln，没法静默调用。
#[derive(Debug, Default)]
pub struct Validation {
    pub problems: Vec<String>,
}

impl Validation {
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }
}

pub fn validate_commit_message(message: &str) -> Validation {
    let mut problems = Vec::new();
    let lines: Vec<&str> = message.lines().collect();

    if lines.is_empty() || lines[0].trim().is_empty() {
        problems.push("消息为空".to_string());
        return Validation { problems };
    }

    let header = lines[0].trim();
    let commit_types = vec![
        "feat", "fix", "docs", "style", "refactor", "perf", "test", "chore", "ci", "build",
        "revert",
    ];
    let types_pattern = commit_types.join("|");
    let pattern = format!(r"^({})(\(.+\))?:\s.+", types_pattern);

    let re = Regex::new(&pattern).unwrap();
    if !re.is_match(header) {
        problems.push(format!(
            "标题格式不正确: {} （期望 <type>(<scope>): <subject>）",
            header
        ));
    }

    // 检查 subject 长度
    //
    // 两个坑：一是按**字符数**而不是字节数——中文一个字占 3 字节，
    // 20 个汉字就有 60 字节，按字节算会把完全合规的中文标题判成超长；
    // 二是要用 split_once 取第一个冒号之后的**全部**内容，
    // 否则 subject 自身含冒号时只会量到第二段，超长也能蒙混过关。
    if let Some((_, subject)) = header.split_once(':') {
        let subject_len = subject.trim().chars().count();
        if subject_len > 50 {
            problems.push(format!("Subject 超过50字符: {}", subject_len));
        }
    }

    // 检查 body 格式
    if lines.len() >= 2 && !lines[1].trim().is_empty() {
        problems.push("标题和 body 之间需要有空行".to_string());
    }

    Validation { problems }
}

/// 把校验问题打印给用户。
pub fn print_validation(validation: &Validation) {
    for problem in &validation.problems {
        eprintln!("{}", format!("⚠️  {problem}").yellow());
    }
}

pub fn commit_with_confirmation(message: &str, auto: bool) -> Result<()> {
    if auto {
        println!("{}", "🚀 自动提交...".blue());
        crate::git::commit_with_message(message)?;
        println!("{}", "✅ 提交成功！".green());
        return Ok(());
    }

    // 显示消息让用户确认
    println!("\n{}", "=".repeat(70).yellow());
    println!(
        "{}",
        "📝 生成的 Commit 消息 (Conventional + Body)：".green()
    );
    println!("{}", "=".repeat(70).yellow());
    println!("{}", message);
    println!("{}", "=".repeat(70).yellow());

    print!("\n是否使用此消息提交？(y/n/e 编辑): ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();

    match input.as_str() {
        "y" => {
            crate::git::commit_with_message(message)?;
            println!("{}", "✅ 提交成功！".green());
            Ok(())
        }
        "e" => {
            let edited = edit_message(message)?;
            if let Some(edited_msg) = edited {
                crate::git::commit_with_message(&edited_msg)?;
                println!("{}", "✅ 提交成功！".green());
            } else {
                println!("{}", "已取消提交".yellow());
            }
            Ok(())
        }
        _ => {
            println!("{}", "已取消提交".yellow());
            Ok(())
        }
    }
}

fn edit_message(message: &str) -> Result<Option<String>> {
    let temp_file = tempfile::NamedTempFile::new()?;
    std::fs::write(temp_file.path(), message)?;
    let path = temp_file.path().to_str().unwrap().to_string();

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".to_string());

    let status = std::process::Command::new(&editor).arg(&path).status()?;

    if !status.success() {
        anyhow::bail!("编辑器退出异常");
    }

    let content = std::fs::read_to_string(&path)?;
    let content = content.trim().to_string();

    if content.is_empty() {
        return Ok(None);
    }

    Ok(Some(content))
}

#[cfg(test)]
mod tests {
    use super::validate_commit_message;

    #[test]
    fn accepts_chinese_subject_under_50_chars() {
        // 20 个汉字 = 60 字节，按字节判断会被误杀
        let message = "feat(ai): 修复摘要生成走非流式通道导致内容为空的问题\n\n- 细节";

        assert!(validate_commit_message(message).ok());
    }

    #[test]
    fn accepts_subject_exactly_50_chars() {
        let message = format!("feat: {}", "字".repeat(50));

        assert!(validate_commit_message(&message).ok());
    }

    #[test]
    fn rejects_subject_over_50_chars() {
        let message = format!("feat: {}", "字".repeat(51));

        assert!(!validate_commit_message(&message).ok());
    }

    /// subject 自身含冒号时，必须量到冒号之后的全部内容，
    /// 而不是被截断成第二段（否则超长也能通过）。
    #[test]
    fn counts_full_subject_when_it_contains_colon() {
        let message = format!("feat: a: {}", "字".repeat(51));

        assert!(!validate_commit_message(&message).ok());
    }

    #[test]
    fn rejects_header_without_type_prefix() {
        assert!(!validate_commit_message("更新了一些东西").ok());
    }

    #[test]
    fn rejects_missing_blank_line_before_body() {
        let message = "feat(ai): 修复摘要生成逻辑\n- 紧跟着的 body";

        assert!(!validate_commit_message(message).ok());
    }

    /// 校验必须能给出可读的问题描述（strict_format 要把它们展示给用户）
    #[test]
    fn reports_every_problem() {
        let message = "随便写的标题\n- 紧跟着的 body";
        let validation = validate_commit_message(message);

        assert!(!validation.ok());
        assert_eq!(validation.problems.len(), 2, "{:?}", validation.problems);
        assert!(validation
            .problems
            .iter()
            .any(|p| p.contains("标题格式不正确")));
        assert!(validation.problems.iter().any(|p| p.contains("空行")));
    }

    #[test]
    fn rejects_empty_message() {
        let validation = validate_commit_message("");

        assert!(!validation.ok());
        assert!(validation.problems[0].contains("消息为空"));
    }
}
