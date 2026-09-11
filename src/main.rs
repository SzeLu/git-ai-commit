// src/main.rs
mod ai;
mod chunk;
mod commit;
mod config;
mod debug;
mod git;

use anyhow::{Context, Result};
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

    /// 打印与模型交互的原始细节（请求参数、HTTP 响应头、每个 SSE 帧），
    /// 全部输出到 stderr
    #[arg(long)]
    debug: bool,
}

/// 生成结果。
struct GeneratedMessage {
    text: String,
    /// 是否走了降级路径（模型没能正常返回，用了本地摘要或本地兜底消息）
    degraded: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    debug::set_enabled(cli.debug);

    // 读取配置文件，支持多模型列表与选定模型
    let mut config = config::Config::load().unwrap_or_default();

    // 没有可用模型（首次运行、配置损坏、或 selected_model 指向已删除的条目）时
    // 交互式补齐。这里往已读到的配置里合并，而不是新建一份，避免把配置文件里
    // 其它模型一起冲掉。
    if config.active_model().is_none() {
        println!("❌ 配置中没有可用的模型，需手动输入模型信息");
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

        config.models.insert(
            model.clone(),
            config::ModelConfig {
                model: model.clone(),
                base_url,
                api_token,
            },
        );
        // 兼容旧配置里的单模型字段
        config.model = model.clone();
        config.selected_model = model;

        if let Err(e) = config.save() {
            eprintln!("❌ 保存配置失败: {}", e);
        }
    }

    // 打印当前实际生效的大模型
    // （`selected_model` 可能已失效而回退到兼容字段 `model`，所以要显示解析结果）
    let model_config = config.active_model().context("配置中没有可用的模型")?;
    println!("📌 当前使用的大模型: {}", model_config.model.red());

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
    let generated = generate_message(
        &config,
        model_config,
        &diff,
        &status,
        &diff_stats,
        &repo_info,
    )
    .await?;

    // 处理结果
    let validation = commit::validate_commit_message(&generated.text);
    let auto = effective_auto(cli.auto, config.auto_commit, cli.dry_run);

    if cli.dry_run {
        println!("\n{}", "=".repeat(70).yellow());
        println!("{}", "📝 生成的 Commit 消息 (Dry Run)：".green());
        println!("{}", "=".repeat(70).yellow());
        println!("{}", generated.text);
        println!("{}", "=".repeat(70).yellow());

        commit::print_validation(&validation);
        if validation.ok() {
            println!("{}", "✅ 格式验证通过".green());
        } else {
            println!("{}", "⚠️  格式验证失败，请检查".yellow());
        }
        return Ok(());
    }

    if !cli.auto && config.auto_commit {
        println!("{}", "⚙️  config.json 中 auto_commit=true，跳过确认".blue());
    }

    // 格式校验前置到提交之前。`strict_format` 决定失败时是阻断还是仅提示。
    if !validation.ok() {
        // 流式输出结尾没有换行，先补一个，免得提示和消息粘在同一行
        eprintln!();
        commit::print_validation(&validation);
        println!("\n{}", "生成的 Commit 消息：".yellow());
        println!("{}", generated.text);

        if config.strict_format {
            if auto {
                eprintln!(
                    "{}",
                    "❌ 生成的 commit 消息未通过格式校验（strict_format = true），已阻止提交".red()
                );
                eprintln!("   可先用 --dry-run 预览，或在 config.json 中设置 strict_format=false");
                std::process::exit(1);
            }
            eprintln!(
                "{}",
                "⚠️  strict_format = true，但当前是交互模式，是否提交由你决定".yellow()
            );
        } else {
            eprintln!(
                "{}",
                "⚠️  格式校验未通过（strict_format = false，仍可提交）".yellow()
            );
        }
    }

    // 降级消息绝不自动提交
    let auto = should_auto_commit(auto, generated.degraded);
    if !auto && (cli.auto || config.auto_commit) {
        eprintln!(
            "{}",
            "⚠️  本次为降级消息（模型未正常返回），强制走确认流程".yellow()
        );
    }

    commit::commit_with_confirmation(&generated.text, auto)?;

    Ok(())
}

/// 生成 commit 消息：短 diff 直接生成，长 diff 走「分块摘要 -> 合并 -> 生成」。
///
/// 摘要失败不再中断整个流程——改用不依赖模型的本地结构化摘要兜底，
/// 保证 `git aic` 始终能产出消息，同时把降级情况明确告诉用户。
async fn generate_message(
    config: &config::Config,
    model_config: &config::ModelConfig,
    diff: &str,
    status: &str,
    diff_stats: &str,
    repo_info: &git::RepoInfo,
) -> Result<GeneratedMessage> {
    let threshold = config.chunk_threshold;

    if diff.len() <= threshold {
        println!("{}", "⚡ 变更规模适中，正在实时生成提交消息...".green());

        let context = ai::CommitContext {
            diff,
            status,
            diff_stats,
            repo_info,
        };
        let text = ai::generate_commit_message_streaming(
            model_config,
            &context,
            config.final_params(),
            print_delta,
        )
        .await?;

        return Ok(GeneratedMessage {
            text,
            degraded: false,
        });
    }

    println!(
        "{}",
        "⚠️  变更内容较长，正在采用“分块总结 -> 合并 -> 生成”机制进行处理...".yellow()
    );
    let chunks = chunk::split_into_chunks(diff, threshold);

    // 记录 (块序号, 失败原因)，用于最后汇总提示
    let mut degraded_blocks: Vec<(usize, String)> = Vec::new();
    let mut sections: Vec<String> = Vec::new();
    // 续段判断要用：上一块最后出现的文件
    let mut last_file: Option<String> = None;

    for (i, piece) in chunks.iter().enumerate() {
        let index = i + 1;
        let total = chunks.len();
        let label = format!("第 {index}/{total} 块摘要");

        // 无论摘要成功与否都要算出结构信息：既做兜底素材，也用来推进续段状态
        let digest = chunk::local_chunk_digest(piece, last_file.as_deref());
        if let Some(file) = digest.files.last() {
            last_file = Some(file.path.clone());
        }

        print!("   [块 {index}/{total}] 正在生成摘要...");
        io::stdout().flush()?;

        match ai::generate_summary(model_config, piece, config.summary_params(), &label).await {
            Ok(summary) => {
                sections.push(format!(
                    "### 块 {index}/{total}（模型摘要）\n{}",
                    summary.trim()
                ));
                println!("{}", " 完成".green());
            }
            Err(err) => {
                // 第一次失败就把完整诊断打出来——里面有帧统计、finish_reason
                // 和原始响应片段，这是定位问题的关键，不能只留一行。
                // 后续块的原因通常相同，只记一行即可，免得刷屏。
                if degraded_blocks.is_empty() {
                    eprintln!("\n{}", err.to_string().yellow());
                }
                degraded_blocks.push((index, first_line(&err)));
                sections.push(format!(
                    "### 块 {index}/{total}（本地结构化摘要，模型未能生成）\n[块 {index}/{total} 本地摘要] {}",
                    digest.render()
                ));
                println!("{}", " ⚠️ 已降级为本地结构化摘要".yellow());
            }
        }
    }

    if !degraded_blocks.is_empty() {
        eprintln!(
            "{}",
            format!(
                "⚠️  有 {}/{} 块使用了本地降级摘要，最终消息质量可能下降",
                degraded_blocks.len(),
                chunks.len()
            )
            .yellow()
        );
        if degraded_blocks.len() > 1 {
            let blocks: Vec<String> = degraded_blocks
                .iter()
                .map(|(index, _)| index.to_string())
                .collect();
            eprintln!("   降级的块: {}", blocks.join(", "));
        }
        eprintln!("   提示: 加 --debug 查看完整原始响应，或在 config.json 中调大 max_tokens");
    }

    let custom_prompt = format!(
        r#"以下是代码变更的分块摘要，请根据这些信息生成一个符合 Conventional Commits 规范且包含详细 Body 的最终 commit 消息。

仓库信息：
- 分支: {}
- 远程仓库: {}

变更统计：
{}

变更状态：
{}

分块摘要：
{}
"#,
        repo_info.branch,
        repo_info.remote,
        diff_stats,
        status,
        sections.join("\n\n")
    );

    println!("{}", "✍️  正在生成最终提交消息...".blue());
    match ai::generate_commit_message_custom_prompt(
        model_config,
        &custom_prompt,
        config.final_params(),
        print_delta,
    )
    .await
    {
        Ok(text) => Ok(GeneratedMessage {
            text,
            degraded: !degraded_blocks.is_empty(),
        }),
        Err(err) => {
            // 最终生成也失败：用本地信息兜底，工具仍然可用
            eprintln!(
                "{}",
                format!("⚠️  最终生成失败，改用本地兜底消息：{}", first_line(&err)).yellow()
            );
            Ok(GeneratedMessage {
                text: local_fallback_message(status),
                degraded: true,
            })
        }
    }
}

/// 完全本地、不依赖模型的兜底消息。
fn local_fallback_message(status: &str) -> String {
    let files: Vec<&str> = status
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();

    let mut body = String::new();
    for line in files.iter().take(20) {
        body.push_str(&format!("- {line}\n"));
    }
    if files.len() > 20 {
        body.push_str(&format!("- …（其余 {} 个文件略）\n", files.len() - 20));
    }

    format!(
        "chore: 更新 {} 个文件\n\n{}（本条消息由本地降级逻辑生成：模型未返回内容）",
        files.len().max(1),
        body
    )
}

/// `--dry-run` 永远优先：预览就绝不提交。
fn effective_auto(cli_auto: bool, config_auto: bool, dry_run: bool) -> bool {
    !dry_run && (cli_auto || config_auto)
}

/// 降级消息（模型没正常返回）绝不自动提交，必须由人过一眼。
fn should_auto_commit(auto: bool, degraded: bool) -> bool {
    auto && !degraded
}

/// 错误信息的第一行，用于单行提示。
fn first_line(err: &anyhow::Error) -> String {
    err.to_string()
        .lines()
        .next()
        .unwrap_or("未知错误")
        .to_string()
}

/// 流式增量文本的默认输出方式。
///
/// 管道下游提前退出（如 `| head`）会让 flush 返回 BrokenPipe，这里忽略即可，
/// 不该因此 panic。
fn print_delta(text: &str) {
    print!("{}", text);
    let _ = io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_never_auto_commits() {
        assert!(!effective_auto(true, true, true));
        assert!(!effective_auto(false, false, true));
    }

    #[test]
    fn auto_commit_comes_from_cli_or_config() {
        assert!(effective_auto(true, false, false));
        assert!(effective_auto(false, true, false));
        assert!(!effective_auto(false, false, false));
    }

    /// 降级消息必须强制确认，即使开了 --auto
    #[test]
    fn degraded_message_never_auto_commits() {
        assert!(!should_auto_commit(true, true));
        assert!(should_auto_commit(true, false));
    }

    #[test]
    fn fallback_message_is_conventional_and_lists_files() {
        let message = local_fallback_message("M  src/ai.rs\nA  src/chunk.rs\n");

        // 必须能通过格式校验，否则会被 strict_format 拦下
        assert!(
            crate::commit::validate_commit_message(&message).ok(),
            "{message}"
        );
        assert!(message.contains("src/ai.rs"));
        assert!(message.contains("src/chunk.rs"));
        assert!(message.contains("本地降级"));
    }

    #[test]
    fn fallback_message_survives_empty_status() {
        let message = local_fallback_message("");

        assert!(
            crate::commit::validate_commit_message(&message).ok(),
            "{message}"
        );
    }
}
