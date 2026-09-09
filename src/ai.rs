use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::git::RepoInfo;

use crate::config;

#[derive(Debug, Serialize)]
struct LlmRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f32,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct LlmResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Option<Message>,
    delta: Option<Message>,
}

/// 核心生成逻辑 (带流式回调)
pub async fn generate_commit_message_streaming<F>(
    config: &config::ModelConfig,
    diff: &str,
    status: &str,
    diff_stats: &str,
    repo_info: &RepoInfo,
    max_tokens: u32,
    temperature: f32,
    mut callback: F,
) -> Result<String>
where
    F: FnMut(String),
{
    let commit_types = vec![
        "feat", "fix", "docs", "style", "refactor", "perf", "test", "chore", "ci", "build",
        "revert",
    ];

    let prompt = format!(
        r#"你是一个专业的 Git commit 消息生成专家。请根据以下代码变更生成一个符合 Conventional Commits 规范且包含详细 Body 的 commit 消息。

仓库信息：
- 分支: {}
- 远程仓库: {}

变更统计：
{}

变更状态：
{}

代码变更详情：
{}

请按照以下格式生成 commit 消息：

<type>(<scope>): <subject>

<body>

<footer>

要求：
1. Type：{}
2. Scope（可选）：影响的范围，如模块名、组件名
3. Subject：简洁描述（不超过50字符，中文，现在时态）
4. Body：详细描述
   - 说明改了什么、为什么改
   - 列出主要变更点（使用 - 或 * 列表）
   - 如果有 Breaking Changes 需要特别说明
   - 使用中文
5. Footer（可选）：关闭的 Issue 或 Breaking Changes

特别注意：
- 分析代码变更，理解其目的和影响
- Body 部分要详细且有价值
- 确保格式严格遵循 Conventional Commits 规范
- 所有描述使用中文
- type 必须是小写字母

请只返回 commit 消息内容，不要包含其他解释。"#,
        repo_info.branch,
        repo_info.remote,
        diff_stats,
        status,
        diff,
        commit_types.join(", ")
    );

    let client = Client::new();
    let request = LlmRequest {
        model: config.model.to_string(),
        messages: vec![
            Message {
                role: "system".to_string(),
                content: r#"你是一个专业的 Git commit 消息生成专家。
你必须**直接输出**最终的 commit 消息，**严禁**进行任何推理、解释或思维链（CoT）分析。
你必须严格按照以下格式输出：

<type>(<scope>): <subject>

<body>

<footer>

其中：
- type 必须是小写字母
- subject 不超过50个字符
- body 要详细描述变更内容
- 各部分之间要有空行分隔"#
                    .to_string(),
            },
            Message {
                role: "user".to_string(),
                content: prompt,
            },
        ],
        temperature,
        max_tokens,
        stream: Some(true),
    };

    let base_url = config.base_url.clone();
    let endpoint = if base_url.ends_with("/chat/completions") {
        base_url.clone()
    } else if base_url.ends_with('/') {
        format!("{}chat/completions", base_url)
    } else {
        format!("{}/chat/completions", base_url)
    };

    let mut response = client
        .post(&endpoint)
        .header("Authorization", format!("Bearer {}", config.api_token))
        .header("Content-Type", "application/json")
        .json(&request)
        .send()
        .await
        .context("调用 大模型 API 失败")?;

    if !response.status().is_success() {
        let error_text = response.text().await?;
        anyhow::bail!("API 请求失败: {}", error_text);
    }

    use futures_util::StreamExt;
    let mut stream = response.bytes_stream();
    let mut full_content = String::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.context("读取响应流失败")?;
        let chunk_str = String::from_utf8_lossy(&chunk);
        
        for line in chunk_str.lines() {
            if line.starts_with("data: ") {
                let json_str = &line[6..];
                if json_str == "[DONE]" {
                    return Ok(full_content);
                }
                if let Ok(res) = serde_json::from_str::<LlmResponse>(json_str) {
                    if let Some(choice) = res.choices.first() {
                        if let Some(delta) = &choice.delta {
                            if let Some(content) = &delta.content {
                                if !content.is_empty() {
                                    full_content.push_str(content);
                                    callback(content.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(full_content)
}

/// 普通版本（非流式，供非交互逻辑使用）
pub async fn generate_commit_message(
    config: &config::ModelConfig,
    diff: &str,
    status: &str,
    diff_stats: &str,
    repo_info: &RepoInfo,
    max_tokens: u32,
    temperature: f32,
) -> Result<String> {
    generate_commit_message_streaming(
        config,
        diff,
        status,
        diff_stats,
        repo_info,
        max_tokens,
        temperature,
        |_| {},
    )
    .await
}

/// 生成摘要
pub async fn generate_summary(
    config: &config::ModelConfig,
    diff_chunk: &str,
) -> Result<String> {
    let client = Client::new();
    let request = LlmRequest {
        model: config.model.to_string(),
        messages: vec![
            Message {
                role: "system".to_string(),
                content: "你是一个代码变更分析专家。请用一句话总结接下来的代码变更内容。".to_string(),
            },
            Message {
                role: "user".to_string(),
                content: diff_chunk.to_string(),
            },
        ],
        temperature: 0.3,
        max_tokens: 200,
        stream: None,
    };

    let base_url = config.base_url.clone();
    let endpoint = if base_url.ends_with("/chat/completions") {
        base_url.clone()
    } else if base_url.ends_with('/') {
        format!("{}chat/completions", base_url)
    } else {
        format!("{}/chat/completions", base_url)
    };

    let response = client
        .post(&endpoint)
        .header("Authorization", format!("Bearer {}", config.api_token))
        .header("Content-Type", "application/json")
        .json(&request)
        .send()
        .await
        .context("调用 摘要生成 API 失败")?;

    let response_data: LlmResponse = response.json().await.context("解析摘要响应失败")?;
    Ok(response_data.choices[0].message.as_ref().and_then(|m| m.content.clone()).unwrap_or_default())
}