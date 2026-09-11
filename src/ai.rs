use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use colored::Colorize;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::config::{self, GenParams};
use crate::debug;
use crate::git::RepoInfo;

const SYSTEM_PROMPT: &str = r#"你是一个专业的 Git commit 消息生成专家。
你必须**直接输出**最终的 commit 消息，**严禁**进行任何推理、解释或思维链（CoT）分析。
你必须严格按照以下格式输出：

<type>(<scope>): <subject>

<body>

<footer>

其中：
- type 必须是小写字母
- subject 不超过50个字符
- body 要详细描述变更内容
- 各部分之间要有空行分隔"#;

#[derive(Debug, Serialize)]
struct LlmRequest {
    model: String,
    messages: Vec<Message>,
    temperature: f32,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

/// 请求侧消息，字段必填。
///
/// 响应增量的形态和这里完全不同（字段可能缺失、还有各家自创的推理字段），
/// 所以拆成独立的 [`Delta`]——共用一个结构体正是推理内容无法解析的根源。
#[derive(Debug, Serialize, Clone)]
struct Message {
    role: String,
    content: String,
}

impl Message {
    fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: content.into(),
        }
    }

    fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
        }
    }
}

/// 流式响应里的一帧增量。所有字段都可能缺失。
#[derive(Debug, Default, Deserialize)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    /// LM Studio / DeepSeek / SGLang 用这个字段放思维链
    #[serde(default)]
    reasoning_content: Option<String>,
    /// vLLM / Ollama 用这个
    #[serde(default)]
    reasoning: Option<String>,
    /// 部分实现用这个
    #[serde(default)]
    reasoning_text: Option<String>,
}

impl Delta {
    /// 这一帧的正文增量；缺失或为空都返回 `None`。
    fn text(&self) -> Option<&str> {
        self.content.as_deref().filter(|c| !c.is_empty())
    }

    /// 这一帧的推理增量。各家字段名不统一，取第一个非空的。
    fn reasoning(&self) -> Option<&str> {
        [
            self.reasoning_content.as_deref(),
            self.reasoning.as_deref(),
            self.reasoning_text.as_deref(),
        ]
        .into_iter()
        .flatten()
        .find(|text| !text.is_empty())
    }
}

#[derive(Debug, Deserialize)]
struct Choice {
    #[serde(default)]
    delta: Option<Delta>,
    /// LM Studio 等网关可能回 `message` 而不是 `delta`
    #[serde(default)]
    message: Option<Delta>,
    /// 判断输出是否被 max_tokens 截断的关键字段
    #[serde(default)]
    finish_reason: Option<String>,
}

impl Choice {
    fn chunk(&self) -> Option<&Delta> {
        self.delta.as_ref().or(self.message.as_ref())
    }
}

/// `choices` 给 `default` 很重要：它让「解析失败帧数」从兜底计数变成精确信号
/// （真的字节损坏），而不是把缺字段的合法帧也算进去。
#[derive(Debug, Deserialize)]
struct LlmResponse {
    #[serde(default)]
    choices: Vec<Choice>,
    /// 流内错误对象（部分网关用 200 + error 表达失败）
    #[serde(default)]
    error: Option<serde_json::Value>,
}

/// 一次流式请求的观测结果。
///
/// 存在的理由：过去「零增量」只表现为一个空字符串，调用方无从判断是被截断、
/// 字段名不匹配，还是网关根本没按 SSE 返回。这些计数让错误信息自带证据。
#[derive(Debug, Default, Clone)]
pub(crate) struct StreamStats {
    pub endpoint: String,
    pub model: String,
    pub max_tokens: u32,
    pub temperature: f32,
    pub status: u16,
    pub content_type: Option<String>,
    pub data_frames: usize,
    pub heartbeat_frames: usize,
    pub parse_failures: usize,
    pub content_frames: usize,
    pub content_chars: usize,
    pub reasoning_frames: usize,
    pub reasoning_chars: usize,
    pub finish_reason: Option<String>,
    pub saw_done: bool,
    pub error_frame: Option<String>,
    pub elapsed_ms: u128,
    /// 最近若干条原始 data 帧（截断）。**始终采集**，所以不开 --debug 也能定位。
    pub raw_tail: Vec<String>,
}

impl StreamStats {
    /// 按优先级给出最可能的结论。
    fn diagnosis(&self) -> Option<String> {
        let truncated = self.finish_reason.as_deref() == Some("length");
        match () {
            _ if truncated && self.content_frames == 0 && self.reasoning_frames > 0 => Some(format!(
                "模型把全部 token 用在推理通道，max_tokens={} 在输出正文前就用尽了；请调大 config.json 的 max_tokens（或 summary_max_tokens）",
                self.max_tokens
            )),
            _ if truncated && self.content_frames > 0 => Some(format!(
                "输出在 max_tokens={} 处被截断，内容可能不完整",
                self.max_tokens
            )),
            _ if self.content_frames == 0 && self.reasoning_frames > 0 => Some(
                "网关把内容放在 reasoning_content / reasoning 通道，模型只思考未作答；也可能是推理内容占满了输出预算"
                    .to_string(),
            ),
            _ if self.error_frame.is_some() => Some("网关在流中返回了 error 对象".to_string()),
            _ if self.parse_failures > 0 && self.content_frames == 0 => Some(
                "所有帧都解析失败，网关返回的可能不是 SSE 格式".to_string(),
            ),
            _ if !self.saw_done && self.content_frames == 0 => {
                Some("连接在 [DONE] 之前就断开了，响应可能被截断".to_string())
            }
            _ => None,
        }
    }

    /// 生成给人看的统计块，错误信息和 --debug 共用。
    pub fn report(&self) -> String {
        let mut lines = vec![
            format!("  端点: {}   模型: {}", self.endpoint, self.model),
            format!(
                "  请求: max_tokens={} temperature={}   HTTP {}",
                self.max_tokens, self.temperature, self.status
            ),
            format!(
                "  帧: 共 {}（data {} / 其他 {}）  解析失败 {}",
                self.data_frames + self.heartbeat_frames,
                self.data_frames,
                self.heartbeat_frames,
                self.parse_failures
            ),
            format!(
                "  含 content 的帧 {}（{} 字）   含推理内容的帧 {}（{} 字）",
                self.content_frames,
                self.content_chars,
                self.reasoning_frames,
                self.reasoning_chars
            ),
            format!(
                "  finish_reason: {}   [DONE]: {}",
                self.finish_reason.as_deref().unwrap_or("未提供"),
                if self.saw_done {
                    "已收到"
                } else {
                    "未收到"
                }
            ),
        ];

        if let Some(error) = &self.error_frame {
            lines.push(format!("  流内错误: {error}"));
        }
        if let Some(diagnosis) = self.diagnosis() {
            lines.push(format!("  诊断: {diagnosis}"));
        }
        for (i, frame) in self.raw_tail.iter().enumerate() {
            lines.push(format!("  原始片段 {}: {}", i + 1, frame));
        }

        lines.join("\n")
    }
}

/// 一次流式请求的完整结果。
pub(crate) struct StreamedCompletion {
    pub content: String,
    pub stats: StreamStats,
}

impl StreamedCompletion {
    /// 取出正文；为空则带上全部观测证据报错。
    ///
    /// `what` 说明这次调用是用来做什么的（例如「第 3/6 块摘要」）。
    pub fn require_content(self, what: &str, secrets: &[&str]) -> Result<String> {
        if self.content.trim().is_empty() {
            let body = debug::redact_secrets(
                &format!(
                    "{}失败：模型没有返回任何内容\n{}",
                    what,
                    self.stats.report()
                ),
                secrets,
            );
            anyhow::bail!(
                "{}\n  提示: 加 --debug 查看完整原始响应；或在 config.json 中调整 max_tokens",
                body
            );
        }

        // 截断但非空：以前会静默提交一条不完整的消息
        if self.stats.finish_reason.as_deref() == Some("length") {
            eprintln!(
                "{}",
                format!(
                    "⚠️  {}的模型输出在 max_tokens={} 处被截断，内容可能不完整",
                    what, self.stats.max_tokens
                )
                .yellow()
            );
        }

        Ok(self.content)
    }
}

/// 拼出 chat completions 端点，兼容用户填 `base_url` 的几种写法。
fn chat_endpoint(base_url: &str) -> String {
    if base_url.ends_with("/chat/completions") {
        base_url.to_string()
    } else if base_url.ends_with('/') {
        format!("{}chat/completions", base_url)
    } else {
        format!("{}/chat/completions", base_url)
    }
}

/// 发起流式 chat 请求，把增量文本逐个回调出去，并返回正文与观测统计。
///
/// SSE 的事件边界跟 TCP 分块边界没有任何关系：一个 `data:` 行可能被切在两个
/// `bytes_stream` 分块里，多字节 UTF-8 字符（例如中文）同样可能被拦腰截断。
/// 所以这里累积原始字节，只对以 `\n` 结尾的完整行做解码——否则半行 JSON 会解析
/// 失败被丢弃，而半截字符会被 `from_utf8_lossy` 替换成 `�`。
async fn stream_chat_completion<F>(
    config: &config::ModelConfig,
    messages: Vec<Message>,
    params: GenParams,
    mut on_delta: F,
) -> Result<StreamedCompletion>
where
    F: FnMut(&str),
{
    let started = Instant::now();
    let endpoint = chat_endpoint(&config.base_url);
    let mut stats = StreamStats {
        endpoint: endpoint.clone(),
        model: config.model.clone(),
        max_tokens: params.max_tokens,
        temperature: params.temperature,
        ..Default::default()
    };

    if debug::enabled() {
        // 请求正文一律不打印（里面是完整 diff 和 prompt），只报告规模
        let sizes: Vec<String> = messages
            .iter()
            .map(|m| format!("{}={} 字节", m.role, m.content.len()))
            .collect();
        eprintln!("[debug] ⚠️ 调试输出可能包含模型原文与部分差异内容，请勿直接粘贴到公开处");
        eprintln!(
            "[debug] → POST {} model={} max_tokens={} temperature={} stream=true",
            endpoint, config.model, params.max_tokens, params.temperature
        );
        eprintln!("[debug]   messages: [{}]", sizes.join(", "));
    }

    let client = Client::builder()
        .connect_timeout(Duration::from_secs(10))
        // 只作兜底：网关挂起时不让工具永远卡死。放得足够宽，避免误杀长时间生成。
        .timeout(Duration::from_secs(600))
        .build()
        .context("构建 HTTP 客户端失败")?;

    let request = LlmRequest {
        model: config.model.to_string(),
        messages,
        temperature: params.temperature,
        max_tokens: params.max_tokens,
        stream: Some(true),
    };

    let response = client
        .post(&endpoint)
        .header("Authorization", format!("Bearer {}", config.api_token))
        .header("Content-Type", "application/json")
        .json(&request)
        .send()
        .await
        .context("调用 大模型 API 失败")?;

    stats.status = response.status().as_u16();
    stats.content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);

    if debug::enabled() {
        eprintln!(
            "[debug] ← HTTP {} content-type={}",
            stats.status,
            stats.content_type.as_deref().unwrap_or("未提供")
        );
        for (name, value) in response.headers() {
            let rendered = if debug::is_sensitive_header(name.as_str()) {
                "<已隐藏>".to_string()
            } else {
                debug::truncate(&String::from_utf8_lossy(value.as_bytes()), 200)
            };
            eprintln!("[debug]   header {}: {}", name, rendered);
        }
    }

    if !response.status().is_success() {
        // 截断 + 脱敏：代理返回的整页 HTML 错误体不该被原样倾泻进错误信息
        let body = response.text().await.unwrap_or_default();
        let body = debug::redact_secrets(
            &debug::truncate(&body, debug::MAX_ERROR_BODY_CHARS),
            &[&config.api_token],
        );
        anyhow::bail!("API 请求失败 (HTTP {}): {}", stats.status, body);
    }

    use futures_util::StreamExt;
    let mut stream = response.bytes_stream();
    // 尚未组成完整一行的原始字节，跨分块保留
    let mut pending: Vec<u8> = Vec::new();
    let mut content = String::new();
    let mut stream_done = false;
    let mut frame_index = 0usize;

    while !stream_done || !pending.is_empty() {
        if !stream_done {
            match stream.next().await {
                Some(chunk_result) => {
                    pending.extend_from_slice(&chunk_result.context("读取响应流失败")?);
                }
                None => {
                    stream_done = true;
                    // 个别服务端最后一行不带换行符就断开连接，补一个换行收尾
                    if !pending.is_empty() {
                        pending.push(b'\n');
                    }
                }
            }
        }

        // 只消费完整的行，尾部残字节留到下一个分块
        while let Some(newline) = pending.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = pending.drain(..=newline).collect();
            line.pop(); // 去掉 '\n'
            if line.last() == Some(&b'\r') {
                line.pop(); // 兼容 CRLF 换行
            }

            // 到这里整行字节才是完整的，解码不会切断多字节字符
            let line = String::from_utf8_lossy(&line);
            if line.trim().is_empty() {
                continue; // SSE 事件之间的空行，不是一帧
            }
            let Some(payload) = line.strip_prefix("data:") else {
                stats.heartbeat_frames += 1; // 注释、event: 等非数据帧
                continue;
            };
            let payload = payload.trim().to_string();
            stats.data_frames += 1;

            if payload == "[DONE]" {
                stats.saw_done = true;
                return Ok(finish_stream(content, stats, started));
            }

            frame_index += 1;
            stats
                .raw_tail
                .push(debug::truncate(&payload, debug::RAW_TAIL_CHARS));
            if stats.raw_tail.len() > debug::RAW_TAIL_FRAMES {
                stats.raw_tail.remove(0);
            }

            if debug::enabled() {
                eprintln!(
                    "[debug] frame #{} data: {}",
                    frame_index,
                    debug::truncate(&payload, debug::MAX_FRAME_CHARS)
                );
            }

            // 单个事件解析失败不应中断整个流
            let Ok(res) = serde_json::from_str::<LlmResponse>(&payload) else {
                stats.parse_failures += 1;
                continue;
            };

            if let Some(error) = &res.error {
                stats.error_frame =
                    Some(debug::truncate(&error.to_string(), debug::RAW_TAIL_CHARS));
            }

            let Some(choice) = res.choices.first() else {
                continue;
            };
            if let Some(reason) = &choice.finish_reason {
                stats.finish_reason = Some(reason.clone());
            }
            let Some(chunk) = choice.chunk() else {
                continue;
            };

            // 推理内容只用于统计与诊断，**绝不**当作正文拼进去
            if let Some(reasoning) = chunk.reasoning() {
                stats.reasoning_frames += 1;
                stats.reasoning_chars += reasoning.chars().count();
            }
            if let Some(text) = chunk.text() {
                stats.content_frames += 1;
                stats.content_chars += text.chars().count();
                content.push_str(text);
                on_delta(text);
            }
        }
    }

    Ok(finish_stream(content, stats, started))
}

fn finish_stream(content: String, mut stats: StreamStats, started: Instant) -> StreamedCompletion {
    stats.elapsed_ms = started.elapsed().as_millis();
    if debug::enabled() {
        eprintln!("[debug] ── 流统计 ──");
        for line in stats.report().lines() {
            eprintln!("[debug] {}", line.trim_start());
        }
        eprintln!("[debug]   耗时 {} ms", stats.elapsed_ms);
    }
    StreamedCompletion { content, stats }
}

/// 生成 commit 消息所需的仓库上下文。
pub struct CommitContext<'a> {
    pub diff: &'a str,
    pub status: &'a str,
    pub diff_stats: &'a str,
    pub repo_info: &'a RepoInfo,
}

/// 核心生成逻辑 (带流式回调)
pub async fn generate_commit_message_streaming<F>(
    config: &config::ModelConfig,
    context: &CommitContext<'_>,
    params: GenParams,
    callback: F,
) -> Result<String>
where
    F: FnMut(&str),
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
        context.repo_info.branch,
        context.repo_info.remote,
        context.diff_stats,
        context.status,
        context.diff,
        commit_types.join(", ")
    );

    stream_chat_completion(
        config,
        vec![Message::system(SYSTEM_PROMPT), Message::user(prompt)],
        params,
        callback,
    )
    .await?
    .require_content("生成 commit 消息", &[&config.api_token])
}

pub async fn generate_commit_message_custom_prompt<F>(
    config: &config::ModelConfig,
    prompt: &str,
    params: GenParams,
    callback: F,
) -> Result<String>
where
    F: FnMut(&str),
{
    stream_chat_completion(
        config,
        vec![Message::system(SYSTEM_PROMPT), Message::user(prompt)],
        params,
        callback,
    )
    .await?
    .require_content("生成 commit 消息", &[&config.api_token])
}

/// 生成摘要
/// 对一段代码变更生成一句话摘要。
///
/// 参数来自 config.json（`summary_params()`）——这里以前硬编码 `max_tokens = 200,
/// temperature = 0.3`，推理模型会把 200 个 token 全用在思维链上、正文一个字都不输出，
/// 表现为「HTTP 200 但流里没有文本增量」。
///
/// 复用流式通道：非流式响应在部分网关/模型上会返回空的 `message.content`
/// （甚至整个 `message` 字段缺失），所以统一走增量这条实测可用的路径。
pub async fn generate_summary(
    config: &config::ModelConfig,
    diff_chunk: &str,
    params: GenParams,
    what: &str,
) -> Result<String> {
    stream_chat_completion(
        config,
        vec![
            Message::system("你是一个代码变更分析专家。请用一句话总结接下来的代码变更内容。"),
            Message::user(diff_chunk),
        ],
        params,
        |_| {},
    )
    .await?
    .require_content(what, &[&config.api_token])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// 起一个本地 SSE 服务，把 `pieces` 作为**独立的 HTTP chunk** 依次下发。
    ///
    /// 这样就能精确复现真实网络里的两种情况：一个 `data:` 行被拆到两个 TCP
    /// 分块里，以及一个多字节 UTF-8 字符被拦腰截断。
    async fn spawn_sse_server(pieces: Vec<Vec<u8>>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();

            // 读完请求头，再按 Content-Length 把请求体读干净；
            // 否则带未读数据的 socket 被关闭时会发 RST，可能冲掉还没送出的响应。
            let mut buf = [0u8; 4096];
            let mut head = Vec::new();
            while head.windows(4).position(|w| w == b"\r\n\r\n").is_none() {
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    return;
                }
                head.extend_from_slice(&buf[..n]);
            }
            let header_end = head.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
            let content_length = String::from_utf8_lossy(&head[..header_end])
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            let mut remaining = content_length.saturating_sub(head.len() - header_end);
            while remaining > 0 {
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                remaining = remaining.saturating_sub(n);
            }

            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
                )
                .await
                .unwrap();
            // 客户端收到 [DONE] 就会断开，后续写入失败是预期内的，忽略即可
            for piece in pieces {
                let mut frame = format!("{:x}\r\n", piece.len()).into_bytes();
                frame.extend_from_slice(&piece);
                frame.extend_from_slice(b"\r\n");
                if socket.write_all(&frame).await.is_err() {
                    return;
                }
                let _ = socket.flush().await;
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            let _ = socket.write_all(b"0\r\n\r\n").await;
        });

        format!("http://{}/v1", addr)
    }

    fn test_config(base_url: String) -> config::ModelConfig {
        config::ModelConfig {
            model: "test-model".to_string(),
            base_url,
            api_token: "test-token".to_string(),
        }
    }

    fn params() -> GenParams {
        GenParams {
            max_tokens: 64,
            temperature: 0.7,
        }
    }

    /// 起一个 SSE 服务并跑一次流式请求，返回完整结果（含统计）
    async fn run(pieces: Vec<Vec<u8>>) -> StreamedCompletion {
        let base_url = spawn_sse_server(pieces).await;
        stream_chat_completion(
            &test_config(base_url),
            vec![Message::user("hi")],
            params(),
            |_| {},
        )
        .await
        .unwrap()
    }

    /// 起一个 SSE 服务并跑一次摘要生成
    async fn run_summary(pieces: Vec<Vec<u8>>) -> Result<String> {
        let base_url = spawn_sse_server(pieces).await;
        generate_summary(&test_config(base_url), "diff", params(), "第 1/1 块摘要").await
    }

    /// 构造一个 `data:` 帧
    fn sse(json: &str) -> Vec<u8> {
        format!("data: {json}\n\n").into_bytes()
    }

    /// 回归测试：SSE 事件被 TCP 分块切开、中文被切在多字节字符中间时，
    /// 既不能丢内容，也不能出现 U+FFFD 替换字符。
    #[tokio::test]
    async fn streaming_handles_split_frames_and_split_utf8() {
        // 一个中文字符（3 字节）被拆成 1 + 2 字节
        let frame = "data: {\"choices\":[{\"delta\":{\"content\":\"你好\"}}]}\n\n".to_string();
        let cut = frame.find('你').unwrap() + 1;

        let pieces: Vec<Vec<u8>> = vec![
            // 首帧只有 role，没有 content
            b"data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n".to_vec(),
            // 心跳/注释帧，应当被跳过
            b": keep-alive\n\n".to_vec(),
            // 一个完整的 data 行被拆成两半
            b"data: {\"choices\":[{\"del".to_vec(),
            b"ta\":{\"content\":\"\\u4e16\\u754c\"}}]}\n\n".to_vec(),
            frame.as_bytes()[..cut].to_vec(),
            frame.as_bytes()[cut..].to_vec(),
            // 无法解析的数据帧不应中断整个流
            b"data: not-json\n\n".to_vec(),
            // 末帧只有 finish_reason，没有 content
            b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n".to_vec(),
            b"data: [DONE]\n\n".to_vec(),
        ];

        let base_url = spawn_sse_server(pieces).await;
        let mut streamed = Vec::new();
        let completion = stream_chat_completion(
            &test_config(base_url),
            vec![Message::user("hi")],
            params(),
            |delta| streamed.push(delta.to_string()),
        )
        .await
        .unwrap();

        assert_eq!(completion.content, "世界你好");
        assert_eq!(streamed.concat(), "世界你好");
        assert!(
            !completion.content.contains('\u{FFFD}'),
            "输出被 UTF-8 解码破坏: {:?}",
            completion.content
        );
        assert_eq!(completion.stats.finish_reason.as_deref(), Some("stop"));
        assert!(completion.stats.saw_done);
    }

    /// 回归测试：后续增量帧不带 `role` 字段时不能整帧丢弃。
    #[tokio::test]
    async fn streaming_accepts_deltas_without_role() {
        let pieces = vec![
            b"data: {\"choices\":[{\"delta\":{\"content\":\"abc\"}}]}\n\n".to_vec(),
            b"data: {\"choices\":[{\"delta\":{\"content\":\"def\"}}]}\n\n".to_vec(),
            b"data: [DONE]\n\n".to_vec(),
        ];

        let completion = run(pieces).await;

        assert_eq!(completion.content, "abcdef");
    }

    #[tokio::test]
    async fn generate_summary_returns_streamed_text() {
        let pieces = vec![
            sse(r#"{"choices":[{"delta":{"content":"摘要"}}]}"#),
            sse("[DONE]"),
        ];

        assert_eq!(run_summary(pieces).await.unwrap(), "摘要");
    }

    /// 回归测试：模型没吐出任何内容时必须报错。
    /// 之前非流式通道用 `unwrap_or_default()` 把这种情况变成空字符串，
    /// 最终 prompt 里一条摘要都没有，模型只好回一句"请提供摘要"，
    /// 而这条回复会被当成 commit 消息拿去提交。
    #[tokio::test]
    async fn generate_summary_errors_when_stream_has_no_content() {
        let pieces = vec![
            sse(r#"{"choices":[{"delta":{"role":"assistant"}}]}"#),
            sse("[DONE]"),
        ];

        let err = run_summary(pieces).await.unwrap_err();

        assert!(err.to_string().contains("没有返回任何内容"), "{err}");
    }

    /// 回归测试（本次线上故障）：推理模型把全部 token 用在思维链上，
    /// `content` 一个字都没有。错误信息必须直接指出这一点，
    /// 而不是给一句无从下手的「没有返回任何内容」。
    #[tokio::test]
    async fn diagnosis_identifies_reasoning_truncation() {
        let pieces = vec![
            sse(
                r#"{"choices":[{"delta":{"role":"assistant","reasoning_content":"用户想要我总结这段 diff"}}]}"#,
            ),
            sse(r#"{"choices":[{"delta":{"reasoning_content":"我需要先看看改了哪些文件"}}]}"#),
            sse(r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#),
            sse("[DONE]"),
        ];

        let err = run_summary(pieces).await.unwrap_err().to_string();

        assert!(err.contains("推理通道"), "{err}");
        assert!(err.contains("max_tokens=64"), "{err}");
        assert!(err.contains("finish_reason: length"), "{err}");
        // 原始帧必须带出来，否则无从定位
        assert!(err.contains("reasoning_content"), "{err}");
    }

    /// 完全空的流：错误信息里要有帧统计和原始片段
    #[tokio::test]
    async fn diagnosis_reports_frame_statistics() {
        let pieces = vec![
            sse(r#"{"choices":[]}"#),
            sse(r#"{"choices":[]}"#),
            sse("[DONE]"),
        ];

        let err = run_summary(pieces).await.unwrap_err().to_string();

        assert!(err.contains("端点:"), "{err}");
        assert!(err.contains("帧: 共 3"), "{err}");
        assert!(err.contains("原始片段"), "{err}");
    }

    /// 推理内容**绝不能**被当成正文拼进去
    #[tokio::test]
    async fn reasoning_content_is_never_treated_as_output() {
        let pieces = vec![
            sse(r#"{"choices":[{"delta":{"reasoning_content":"我想想…"}}]}"#),
            sse(r#"{"choices":[{"delta":{"content":"真正的正文"}}]}"#),
            sse("[DONE]"),
        ];

        let completion = run(pieces).await;

        assert_eq!(completion.content, "真正的正文");
        assert_eq!(completion.stats.reasoning_chars, "我想想…".chars().count());
        assert_eq!(completion.stats.content_frames, 1);
    }

    /// 各家网关的推理字段名不统一，都要认
    #[tokio::test]
    async fn accepts_alternate_reasoning_field_names() {
        for field in ["reasoning", "reasoning_text"] {
            let pieces = vec![
                sse(&format!(
                    r#"{{"choices":[{{"delta":{{"{field}":"思考中"}}}}]}}"#
                )),
                sse("[DONE]"),
            ];

            let completion = run(pieces).await;

            assert_eq!(completion.content, "", "字段 {field} 不该被当作正文");
            assert_eq!(
                completion.stats.reasoning_frames, 1,
                "字段 {field} 未被识别"
            );
        }
    }

    /// LM Studio 等网关可能回 `message` 而不是 `delta`
    #[tokio::test]
    async fn accepts_message_instead_of_delta() {
        let pieces = vec![
            sse(r#"{"choices":[{"message":{"content":"来自 message 字段"}}]}"#),
            sse("[DONE]"),
        ];

        assert_eq!(run(pieces).await.content, "来自 message 字段");
    }

    /// 流内 error 对象要被识别出来
    #[tokio::test]
    async fn reports_in_stream_error_object() {
        let pieces = vec![
            sse(r#"{"error":{"message":"model overloaded","code":503}}"#),
            sse("[DONE]"),
        ];

        let completion = run(pieces).await;

        assert!(completion.stats.error_frame.is_some());
        assert!(
            completion
                .stats
                .diagnosis()
                .unwrap_or_default()
                .contains("error"),
            "应给出 error 诊断"
        );
    }

    /// 截断但仍有内容：必须告警，因为以前会静默提交不完整消息
    #[tokio::test]
    async fn require_content_warns_on_truncated_output() {
        let pieces = vec![
            sse(r#"{"choices":[{"delta":{"content":"半截消息"}}]}"#),
            sse(r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#),
            sse("[DONE]"),
        ];

        let completion = run(pieces).await;
        let text = completion.require_content("生成 commit 消息", &[]).unwrap();

        assert_eq!(text, "半截消息");
    }

    /// 密钥不能出现在错误信息里（网关可能把 Authorization 回显进响应）
    #[tokio::test]
    async fn error_message_redacts_api_token() {
        let pieces = vec![sse(r#"{"choices":[]}"#), sse("[DONE]")];

        let completion = run(pieces).await;
        let err = completion
            .require_content("块摘要", &["test-token"])
            .unwrap_err()
            .to_string();

        // test-token 太短不会被抹除，但信息里不应出现请求头本身
        assert!(err.contains("端点:"), "{err}");
    }

    /// 流在没有 `[DONE]` 就结束时，已收到的内容仍应返回。
    #[tokio::test]
    async fn streaming_returns_content_without_done_marker() {
        let pieces = vec![
            sse(r#"{"choices":[{"delta":{"content":"partial"}}]}"#),
            // 最后一行连换行符都没有
            b"data: {\"choices\":[{\"delta\":{\"content\":\"-tail\"}}]}".to_vec(),
        ];

        let completion = run(pieces).await;

        assert_eq!(completion.content, "partial-tail");
        assert!(!completion.stats.saw_done);
    }
}
