mod git;
mod ai;
mod commit;
mod config;

use clap::{Parser, Subcommand};
use colored::*;
use anyhow::Result;

#[derive(Parser)]
#[command(name = "git-ai-commit")]
#[command(about = "使用 DeepSeek AI 生成 Conventional Commits + Body 格式的 Git commit 消息", long_about = None)]
#[command(version = "1.0.0")]
struct Cli {
    /// DeepSeek API Key (也可通过环境变量 DEEPSEEK_API_KEY 设置)
    #[arg(long, env = "DEEPSEEK_API_KEY")]
    api_key: Option<String>,

    /// 使用的模型 (默认: deepseek-chat)
    #[arg(long, default_value = "deepseek-chat")]
    model: String,

    /// 自动提交，不进行确认
    #[arg(long, short = 'a')]
    auto: bool,

    /// 包含所有变更（包括未暂存的）
    #[arg(long)]
    all: bool,

    /// 只生成消息，不提交
    #[arg(long)]
    dry_run: bool,

    /// 最大 token 数
    #[arg(long, default_value = "1000")]
    max_tokens: u16,

    /// 温度参数 (0.0 - 1.0)
    #[arg(long, default_value = "0.7")]
    temperature: f32,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

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
            eprintln!("{}", "❌ 没有检测到暂存的变更，请先使用 'git add' 添加文件".red());
            eprintln!("   或使用 --all 参数包含所有变更");
        }
        std::process::exit(1);
    }

    // 获取其他信息
    let status = git::get_git_status()?;
    let diff_stats = git::get_git_diff_stats()?;
    let repo_info = git::get_repo_info()?;

    // 生成 commit 消息
    println!("{}", "🤖 正在生成 commit 消息...".blue());
    
    let api_key = cli.api_key.as_deref();
    let commit_msg = ai::generate_commit_message(
        api_key,
        &cli.model,
        &diff,
        &status,
        &diff_stats,
        &repo_info,
        cli.max_tokens,
        cli.temperature,
    ).await?;

    // 处理结果
    if cli.dry_run {
        println!("\n{}", "=".repeat(70).yellow());
        println!("{}", "📝 生成的 Commit 消息 (Dry Run)：".green());
        println!("{}", "=".repeat(70).yellow());
        println!("{}", commit_msg);
        println!("{}", "=".repeat(70).yellow());
        
        // 验证格式
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
