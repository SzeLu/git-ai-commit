// src/main.rs
mod ai;
mod chunk;
mod commit;
mod config;
mod debug;
mod git;
// 用 `pub` 修饰：i18n 的调用点要到「替换硬编码文案」那一步才接进来，
// 私有模块会让整块尚未被调用的 API 触发 dead_code 警告。
pub mod i18n;

use anyhow::{Context, Result};
use clap::Parser;
use colored::*;
use std::io::{self, Write};

use crate::i18n::{t, t_args};

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

    // 装载界面语言。必须在这里、在**任何输出之前**：晚一步的话，先打印的消息
    // 拿不到资源，只会把 key 原样吐出来。语言来自配置，配置没写则由 i18n
    // 走系统 locale（spec §4 的优先级链）。
    i18n::install(config.language.clone());

    // 没有可用模型（首次运行、配置损坏、或 selected_model 指向已删除的条目）时
    // 交互式补齐。这里往已读到的配置里合并，而不是新建一份，避免把配置文件里
    // 其它模型一起冲掉。
    if config.active_model().is_none() {
        println!("{}", t("config_model_required"));
        let model = prompt_input(&t("model_input_prompt"));
        // Validate input: model, base_url, and api_token must not be empty
        if model.trim().is_empty() {
            eprintln!("{}", t("model_name_validation"));
            std::process::exit(1);
        }
        let base_url = prompt_input(&t("base_url_input_prompt"));
        // Validate input: base_url must not be empty
        if base_url.trim().is_empty() {
            eprintln!("{}", t("base_url_validation"));
            std::process::exit(1);
        }
        let api_token = prompt_input(&t("api_token_input_prompt"));
        // Validate input: api_token must not be empty
        if api_token.trim().is_empty() {
            eprintln!("{}", t("api_token_validation"));
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
            eprintln!(
                "{}",
                t_args("save_config_error", &[("error", e.to_string().into())])
            );
        }
    }

    // 打印当前实际生效的大模型
    // （`selected_model` 可能已失效而回退到兼容字段 `model`，所以要显示解析结果）
    let model_config = config.active_model().context(t("no_active_model_error"))?;
    // 红色只加在模型名上，所以先着色再作为变量传进去，整行不能着色
    println!(
        "{}",
        t_args(
            "current_model_label",
            &[("model", model_config.model.red().to_string().into())]
        )
    );

    // 检查是否在 Git 仓库中
    if !git::is_git_repo()? {
        eprintln!("{}", t("not_git_repo").red());
        std::process::exit(1);
    }

    // 获取变更
    println!("{}", t("analyzing_changes").blue());
    let diff = git::get_git_diff(cli.all)?;

    if diff.is_empty() {
        if cli.all {
            eprintln!("{}", t("no_changes_detected").red());
        } else {
            eprintln!("{}", t("staged_changes_required").red());
            eprintln!("{}", t("use_all_flag_hint"));
        }
        std::process::exit(1);
    }

    // 获取其他信息
    let status = git::get_git_status(cli.all)?;
    let diff_stats = git::get_git_diff_stats(cli.all)?;
    let repo_info = git::get_repo_info()?;

    // 生成 commit 消息
    println!("{}", t("generating_message").blue());
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
        println!("{}", t("dry_run_label").green());
        println!("{}", "=".repeat(70).yellow());
        println!("{}", generated.text);
        println!("{}", "=".repeat(70).yellow());

        commit::print_validation(&validation);
        if validation.ok() {
            println!("{}", t("format_validation_passed").green());
        } else {
            println!("{}", t("format_validation_failed").yellow());
        }
        return Ok(());
    }

    if !cli.auto && config.auto_commit {
        println!("{}", t("config_auto_commit").blue());
    }

    // 格式校验前置到提交之前。`strict_format` 决定失败时是阻断还是仅提示。
    if !validation.ok() {
        // 流式输出结尾没有换行，先补一个，免得提示和消息粘在同一行
        eprintln!();
        commit::print_validation(&validation);

        if config.strict_format {
            if auto {
                eprintln!("{}", t("strict_format_blocked").red());
                eprintln!("{}", t("strict_format_blocked_hint"));
                std::process::exit(1);
            }
            eprintln!("{}", t("format_warning_strict").yellow());
        } else {
            eprintln!("{}", t("format_warning_nonstrict").yellow());
        }
    }

    // 降级消息绝不自动提交
    let auto = should_auto_commit(auto, generated.degraded);
    if !auto && (cli.auto || config.auto_commit) {
        eprintln!("{}", t("degraded_confirmation_required").yellow());
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
    // `Config::load()` 必定补上语言，为 `None` 只可能是直接构造的配置；
    // 退回 `Config::default()` 的缺省值，免得 prompt 里出现空语言名。
    let language = config.language.as_deref().unwrap_or("zh-CN");

    if diff.len() <= threshold {
        println!("{}", t("short_diff_generating").green());

        let context = ai::CommitContext {
            diff,
            status,
            diff_stats,
            repo_info,
        };
        let text = ai::generate_commit_message_streaming(
            config,
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

    println!("{}", t("long_diff_warning").yellow());
    let chunks = chunk::split_into_chunks(diff, threshold);

    // 记录 (块序号, 失败原因)，用于最后汇总提示
    let mut degraded_blocks: Vec<(usize, String)> = Vec::new();
    let mut sections: Vec<String> = Vec::new();
    // 续段判断要用：上一块最后出现的文件
    let mut last_file: Option<String> = None;

    for (i, piece) in chunks.iter().enumerate() {
        let index = i + 1;
        let total = chunks.len();
        let label = t_args(
            "chunk_summary",
            &[("index", index.into()), ("total", total.into())],
        );

        // 无论摘要成功与否都要算出结构信息：既做兜底素材，也用来推进续段状态
        let digest = chunk::local_chunk_digest(piece, last_file.as_deref());
        if let Some(file) = digest.files.last() {
            last_file = Some(file.path.clone());
        }

        print!(
            "{}",
            t_args(
                "chunk_progress",
                &[("index", index.into()), ("total", total.into())]
            )
        );
        io::stdout().flush()?;

        match ai::generate_summary(
            model_config,
            language,
            piece,
            config.summary_params(),
            &label,
        )
        .await
        {
            Ok(summary) => {
                sections.push(format!(
                    "### 块 {index}/{total}（模型摘要）\n{}",
                    summary.trim()
                ));
                println!("{}", t("chunk_complete").green());
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
                println!("{}", t("chunk_degraded").yellow());
            }
        }
    }

    if !degraded_blocks.is_empty() {
        eprintln!(
            "{}",
            t_args(
                "degradation_warning",
                &[
                    ("count", degraded_blocks.len().into()),
                    ("total", chunks.len().into())
                ]
            )
            .yellow()
        );
        if degraded_blocks.len() > 1 {
            let blocks: Vec<String> = degraded_blocks
                .iter()
                .map(|(index, _)| index.to_string())
                .collect();
            eprintln!(
                "{}",
                t_args("block_details", &[("blocks", blocks.join(", ").into())])
            );
        }
        eprintln!("{}", t("debug_hint"));
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

要求：所有内容（subject、body、footer）必须使用 {} 书写。
"#,
        repo_info.branch,
        repo_info.remote,
        diff_stats,
        status,
        sections.join("\n\n"),
        language
    );

    println!("{}", t("final_message").blue());
    match ai::generate_commit_message_custom_prompt(
        model_config,
        language,
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
                t_args(
                    "final_generation_failed",
                    &[("error", first_line(&err).into())]
                )
                .yellow()
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
        // 键值里没有末尾换行，行尾的 \n 是排版，留在调用处
        body.push_str(&format!(
            "{}\n",
            t_args(
                "fallback_more_files",
                &[("count", (files.len() - 20).into())]
            )
        ));
    }

    t_args(
        "fallback_message",
        &[
            // `chore:` 前缀在译文里也保持原样：commit.rs 的校验只认这些 type
            ("count", files.len().max(1).into()),
            ("body", body.into()),
        ],
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
        .map(ToString::to_string)
        .unwrap_or_else(|| t("unknown_error"))
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
        // 下面断言的是中文原文，先把进程 locale 钉到 zh-CN（R7）
        crate::i18n::pin_test_locale();

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

    /// 兜底消息的行形状必须和原来的字面量逐字节一致。
    ///
    /// 这条消息是**要写进仓库的**，而 Fluent 会吃掉多行值的行首空白——`chore:` 后面
    /// 那个空行、以及「其余 N 个文件略」那一行的结尾换行，都得原样落下来。
    #[test]
    fn fallback_message_keeps_the_original_line_shape() {
        crate::i18n::pin_test_locale();

        let message = local_fallback_message("M  src/ai.rs\nA  src/chunk.rs\n");
        assert_eq!(
            message,
            "chore: 更新 2 个文件\n\n- M  src/ai.rs\n- A  src/chunk.rs\n\
             （本条消息由本地降级逻辑生成：模型未返回内容）"
        );

        // 超过 20 个文件时，「其余 N 个」那一行的结尾换行留在调用处，不能丢
        let many: String = (1..=21).map(|i| format!("M  f{i}.rs\n")).collect();
        let message = local_fallback_message(&many);
        assert!(
            message.contains("- …（其余 1 个文件略）\n（本条消息由本地降级逻辑生成"),
            "{message}"
        );
    }

    #[test]
    fn fallback_message_survives_empty_status() {
        // 兜底消息与校验问题的文案都随 locale 变，钉住 zh-CN 让这条与其它测试同语言
        crate::i18n::pin_test_locale();

        let message = local_fallback_message("");

        assert!(
            crate::commit::validate_commit_message(&message).ok(),
            "{message}"
        );
    }
}
