use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::git::RepoInfo;

#[derive(Debug, Serialize)]
struct DeepSeekRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f32,
    max_tokens: u16,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct DeepSeekResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Message,
}

pub async fn generate_commit_message(
    api_key: Option<&str>,
    model: &str,
    diff: &str,
    status: &str,
    diff_stats: &str,
    repo_info: &RepoInfo,
    max_tokens: u16,
    temperature: f32,
) -> Result<String> {
    // 修复：先获取环境变量并持有所有权
    let env_key = std::env::var("DEEPSEEK_API_KEY").ok();
    let api_key = api_key
        .map(String::from)
        .or(env_key)
        .context("请设置 DEEPSEEK_API_KEY 环境变量或通过 --api-key 参数提供")?;

    let commit_types = vec![
        "feat", "fix", "docs", "style", "refactor",
        "perf", "test", "chore", "ci", "build", "revert"
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
    let request = DeepSeekRequest {
        model: model.to_string(),
        messages: vec![
            Message {
                role: "system".to_string(),
                content: r#"你是一个专业的 Git commit 消息生成专家。
你必须严格按照以下格式输出：
<type>(<scope>): <subject>

<body>

<footer>

其中：
- type 必须是小写字母
- subject 不超过50个字符
- body 要详细描述变更内容
- 各部分之间要有空行分隔"#.to_string(),
            },
            Message {
                role: "user".to_string(),
                content: prompt,
            },
        ],
        temperature,
        max_tokens,
    };

    let response = client
        .post("https://api.deepseek.com/chat/completions")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&request)
        .send()
        .await
        .context("调用 DeepSeek API 失败")?;

    if !response.status().is_success() {
        let error_text = response.text().await?;
        anyhow::bail!("API 请求失败: {}", error_text);
    }

    let response_data: DeepSeekResponse = response
        .json()
        .await
        .context("解析 API 响应失败")?;

    let message = response_data
        .choices
        .first()
        .context("API 返回空响应")?
        .message
        .content
        .clone();

    Ok(message)
}
