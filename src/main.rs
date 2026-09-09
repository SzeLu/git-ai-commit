// src/main.rs
mod ai;
mod commit;
mod config;
mod git;

use anyhow::Result;
use clap::Parser;
use colored::*;
use std::io::{self, Write};

fn prompt_input(prompt: &str) -> String {
    print!("{} ", prompt);
    io::stdout().flush().unwrap();
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap_or(0);
    input.trim().to_string()
}

/// CLI 入口
#[derive(Parser)]
#[command(name = "git-ai-commit")]
#[command(about = "使用 AI 生成 Conventional Commits + Body 格式的 Git commit 消息")]
#[command(version = "1.0.0")]
struct Cli {
    /// 自动提交，不进行确认
    #[arg(long, short = 'a')]
    auto: bool,

    /// 包含所有变更（包括未暂存的）
    #[arg(long)]
    all: bool,

    /// 只生成消息，不提交
    #[arg(long)]
    dry_run: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // 读取配置文件，支持多模型列表与选定模型
    let config = config::Config::load().unwrap_or_else(|| {
        // 配置文件缺失或加载失败，交互获取模型信息
        println!("❌ 配置文件未找到或加载失败，需手动输入模型信息");
        let model = prompt_input("请输入模型名称 (e.g., deepseek-chat):");
        // Validate input: model, base_url, and api_token must not be empty
        if model.trim().is_empty() {
            eprintln!("❌ 模型名称不能为空。终止操作。");
            std::process::exit(1);
        }
        let base_url = prompt_input("请输入模型 API URL:");
        // Validate input: base_url must not be empty
        if base_url.trim().is_empty() {
            eprintln!("❌ 模型 API URL 不能为空。终止操作。");
            std::process::exit(1);
        }
        let api_token = prompt_input("请输入 API Token:");
        // Validate input: api_token must not be empty
        if api_token.trim().is_empty() {
            eprintln!("❌ API Token 不能为空。终止操作。");
            std::process::exit(1);
        }
        let mut cfg = config::Config::default();
        cfg.models.insert(
            model.clone(),
            config::ModelConfig {
                model: model.clone(),
                base_url,
                api_token,
            },
        );
        cfg.selected_model = model;
        if let Err(e) = cfg.save() {
            eprintln!("❌ 保存配置失败: {}", e);
        }
        cfg
    });

    // 打印当前使用的大模型
    println!("📌 当前使用的大模型: {}", config.selected_model.red());

    // 检查是否在 Git 仓库中
    if !git::is_git_repo()? {
        eprintln!("{}", "❌ 当前目录不是 Git 仓库".red());
        std::process::exit(1);
    }

    // 获取变更
    println!("{}", "📊 分析代码变更...".blue());
    let diff = git::get_git_diff(cli.all)?;

    if diff.is_empty() {
        if cli.all {
            eprintln!("{}", "❌ 没有检测到任何变更".red());
        } else {
            eprintln!(
                "{}",
                "❌ 没有检测到暂存的变更，请先使用 'git add' 添加文件".red()
            );
            eprintln!("   或使用 --all 参数包含所有变更");
        }
        std::process::exit(1);
    }

    // 获取其他信息
    let status = git::get_git_status(cli.all)?;
    let diff_stats = git::get_git_diff_stats(cli.all)?;
    let repo_info = git::get_repo_info()?;

    // 生成 commit 消息
    println!("{}", "🤖 正在分析变更...".blue());

    let model_config = config.models.get(&config.selected_model).unwrap();

    // 设定分块阈值（字符数）
    let chunk_threshold = 8000;

    let mut commit_msg = if diff.len() > chunk_threshold {
        println!("{}", "⚠️  变更内容较长，正在采用“分块总结 -> 合并 -> 生成”机制进行处理...".yellow());
        let mut chunks = Vec::new();
        let mut current_pos = 0;
        let diff_bytes = diff.as_bytes();
        
        while current_pos < diff_bytes.len() {
            let end = std::cmp::min(current_pos + chunk_size_safe(diff_bytes.len(), chunk_threshold), diff_bytes.len());
            let mut actual_end = end;
            while actual_end > current_pos && diff_bytes[actual_end - 1] != b'\n' {
                actual_end -= 1;
            }
            if actual_end == current_pos {
                actual_end = end;
            }
            
            chunks.push(&diff[current_pos..actual_end]);
            current_pos = actual_end;
        }

        let mut summaries = Vec::new();
        for (i, chunk) in chunks.iter().enumerate() {
            print!("   [块 {}/{}] 正在生成摘要...", i + 1, chunks.len());
            io::stdout().flush()?;
            let summary = ai::generate_summary(&model_config, chunk).await?;
            summaries.push(summary);
            println!("{}", " 完成".green());
        }

        let combined_summaries = summaries.join("\n");
        let custom_prompt = format!(
            "以下是代码变更的分块摘要，请根据这些信息生成一个符合 Conventional Commits 规范且包含详细 Body 的最终 commit 消息：\n\n{}",
            combined_summaries
        );
        
        ai::generate_commit_message_custom_prompt(
            &model_config,
            &custom_prompt,
            config.max_tokens,
            config.temperature,
        ).await?
    } else {
        println!("{}", "⚡ 变更规模适中，正在实时生成提交消息...".green());
        
        ai::generate_commit_message_streaming(
            &model_config,
            &diff,
            &status,
            &diff_stats,
            &repo_info,
            config.max_tokens,
            config.temperature,
            |chunk| {
                print!("{}", chunk);
                io::stdout().flush().unwrap();
            },
        ).await?
    };

    // 处理结果
    if cli.dry_run {
        println!("\n{}", "=".repeat(70).yellow());
        println!("{}", "📝 生成的 Commit 消息 (Dry Run)：".green());
        println!("{}", "=".repeat(70).yellow());
        println!("{}", commit_msg);
        println!("{}", "=".repeat(70).yellow());

        if commit::validate_commit_message(&commit_msg) {
            println!("{}", "✅ 格式验证通过".green());
        } else {
            println!("{}", "⚠️  格式验证失败，请检查".yellow());
        }
    } else {
        commit::commit_with_confirmation(&commit_msg, cli.auto)?;
    }

    Ok(())
}

fn chunk_size_safe(total_len: usize, chunk_size: usize) -> usize {
    if total_len < chunk_size {
        total_len
    } else {
        chunk_size
    }
}

