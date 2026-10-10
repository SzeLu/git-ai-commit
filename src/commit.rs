use crate::i18n::{t, t_args};
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

    // 只认「整条消息都是空白」为空消息。流式返回常常以 `\n\n` 开头，
    // 而 `git commit` 默认 cleanup=strip 会去掉这些前导空行——若把首行空行
    // 也当成空消息，合规的消息会被判不合规，还会顺带跳过后面所有检查。
    let Some(header_index) = lines.iter().position(|line| !line.trim().is_empty()) else {
        problems.push(t("empty_message"));
        return Validation { problems };
    };

    let header = lines[header_index].trim();
    let commit_types = vec![
        "feat", "fix", "docs", "style", "refactor", "perf", "test", "chore", "ci", "build",
        "revert",
    ];
    let types_pattern = commit_types.join("|");
    let pattern = format!(r"^({})(\(.+\))?:\s.+", types_pattern);

    let re = Regex::new(&pattern).unwrap();
    if !re.is_match(header) {
        problems.push(t_args(
            "invalid_header_format",
            &[("header", header.into())],
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
            problems.push(t_args(
                "subject_too_long",
                &[("length", subject_len.into())],
            ));
        }
    }

    // 检查 body 格式：header 的**下一行**必须是空行。
    // 索引要跟着 header 走，否则 header 前有空行时这里会看错行。
    if let Some(next) = lines.get(header_index + 1) {
        if !next.trim().is_empty() {
            problems.push(t("missing_blank_line"));
        }
    }

    Validation { problems }
}

/// 把校验问题打印给用户。
pub fn print_validation(validation: &Validation) {
    for problem in &validation.problems {
        // `problem` 本身已经是本地化过的校验问题，这里只是给它套上提示前缀
        eprintln!(
            "{}",
            t_args("validation_issues", &[("issue", problem.as_str().into())]).yellow()
        );
    }
}

pub fn commit_with_confirmation(message: &str, auto: bool) -> Result<()> {
    if auto {
        println!("{}", t("commit_confirmation").blue());
        crate::git::commit_with_message(message)?;
        println!("{}", t("commit_success").green());
        return Ok(());
    }

    // 显示消息让用户确认
    println!("\n{}", "=".repeat(70).yellow());
    println!("{}", t("commit_message_banner").green());

    // 键值里没有开头那个换行，它属于排版，留在调用处
    print!("\n{}", t("edit_message_prompt"));
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();

    match input.as_str() {
        "y" => {
            crate::git::commit_with_message(message)?;
            println!("{}", t("commit_success").green());
            Ok(())
        }
        "e" => {
            let edited = edit_message(message)?;
            if let Some(edited_msg) = edited {
                crate::git::commit_with_message(&edited_msg)?;
                println!("{}", t("commit_success").green());
            } else {
                println!("{}", t("commit_cancelled").yellow());
            }
            Ok(())
        }
        _ => {
            println!("{}", t("commit_cancelled").yellow());
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
        anyhow::bail!(t("editor_exit_error"));
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
    use super::{t, validate_commit_message};

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
        // 校验问题现在是本地化文案，断言中文就得先钉住 zh-CN（R7）
        crate::i18n::pin_test_locale();

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
        crate::i18n::pin_test_locale();

        let validation = validate_commit_message("");

        assert!(!validation.ok());
        assert!(validation.problems[0].contains("消息为空"));
    }

    /// 只有整条消息都是空白才算空消息——空字符串只是其中一个特例
    #[test]
    fn rejects_whitespace_only_message() {
        crate::i18n::pin_test_locale();

        let validation = validate_commit_message("\n\n   \n\t\n");

        assert!(!validation.ok());
        assert!(validation.problems[0].contains("消息为空"));
    }

    /// 回归测试：正文前导空行不是「消息为空」。
    ///
    /// 流式返回常以 `\n\n` 开头，`git commit` 又会按 cleanup=strip 去掉它们，
    /// 所以首行空行曾经让完全合规的消息飘出「⚠️ 消息为空」——
    /// 交互模式只是碍眼，`--auto` + strict_format 会直接拒绝提交。
    #[test]
    fn accepts_leading_blank_lines() {
        crate::i18n::pin_test_locale();

        let message = "\n\nfeat(ai): 修复摘要生成逻辑\n\n- 细节";
        let validation = validate_commit_message(message);

        assert!(validation.ok(), "{:?}", validation.problems);
    }

    /// 前导空行不能连带吞掉真正的格式问题：判断空行要看 header 的**下一行**，
    /// 而不是消息的第一行。
    #[test]
    fn still_reports_problems_after_leading_blank_lines() {
        crate::i18n::pin_test_locale();

        let message = "\n\n更新了一些东西\n- 紧跟着的 body";
        let validation = validate_commit_message(message);

        assert_eq!(validation.problems.len(), 2, "{:?}", validation.problems);
        assert!(validation
            .problems
            .iter()
            .any(|p| p.contains("标题格式不正确")));
        assert!(validation.problems.iter().any(|p| p.contains("空行")));
    }

    /// 前导空行下的 50 字上限仍按 header 行来量（别量错行）
    #[test]
    fn counts_subject_on_header_line_after_leading_blank_lines() {
        let message = format!("\n\nfeat: {}", "字".repeat(51));

        assert!(!validate_commit_message(&message).ok());
    }

    /// 确认提示语结尾那个空格是原文的一部分（用户在提示后面直接敲 y/n/e），
    /// 而开头那个换行不是——它是排版，留在调用处的 `print!("\n{}", ...)` 里。
    #[test]
    fn confirmation_prompt_keeps_its_trailing_space() {
        crate::i18n::pin_test_locale();

        assert_eq!(
            t("edit_message_prompt"),
            "是否使用此消息提交？(y/n/e 编辑): "
        );
    }
}
