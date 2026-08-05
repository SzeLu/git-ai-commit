use anyhow::Result;
use colored::*;
use regex::Regex;
use std::io::{self, Write};

pub fn validate_commit_message(message: &str) -> bool {
    let lines: Vec<&str> = message.lines().collect();
    
    if lines.is_empty() || lines[0].trim().is_empty() {
        return false;
    }
    
    let header = lines[0].trim();
    let commit_types = vec![
        "feat", "fix", "docs", "style", "refactor",
        "perf", "test", "chore", "ci", "build", "revert"
    ];
    let types_pattern = commit_types.join("|");
    let pattern = format!(r"^({})(\(.+\))?:\s.+", types_pattern);
    
    let re = Regex::new(&pattern).unwrap();
    if !re.is_match(header) {
        eprintln!("{} {}", "⚠️  标题格式不正确:", header.yellow());
        eprintln!("   期望格式: <type>(<scope>): <subject>");
        return false;
    }
    
    // 检查 subject 长度
    if let Some(subject) = header.split(':').nth(1) {
        if subject.trim().len() > 50 {
            eprintln!("{} {}", "⚠️  Subject 超过50字符:", subject.trim().len().to_string().yellow());
            return false;
        }
    }
    
    // 检查 body 格式
    if lines.len() >= 2 && !lines[1].trim().is_empty() {
        eprintln!("{}", "⚠️  标题和 body 之间需要有空行".yellow());
        return false;
    }
    
    true
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
    println!("{}", "📝 生成的 Commit 消息 (Conventional + Body)：".green());
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
    
    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()?;
    
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
