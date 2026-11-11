// src/model_limits.rs
//! 尽力探测当前模型的 token 上限。
//!
//! OpenAI 兼容的 `GET /v1/models` 只列模型名，不含任何 token 限制：官方文档里
//! 每个模型对象只有 `id / created / owned_by / shutdown_date`。上下文长度和最大
//! 输出 token 都是各家网关塞在自己私有端点里的扩展字段，端点、层级、名字互不统一
//! （LM Studio `/api/v1/models`、OpenRouter `/api/v1/models`、LiteLLM `/model/info`）。
//!
//! 所以这里的原则是「能问到就问，问不到就让服务端自己决定」：探测失败不是错误，
//! 请求里省略 `max_tokens` 即可。这样切换模型不需要再动 config.json。

use std::time::Duration;

use reqwest::Client;
use serde_json::Value;

use crate::config::ModelConfig;
use crate::debug;

/// 一次探测得到的模型能力。全是 `Option`：任何一项都可能问不到。
///
/// `Copy` 是为了能塞进 [`crate::config::GenParams`]，让每条请求路径都自带模型能力，
/// 不必层层追加参数。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ModelLimits {
    /// 上下文窗口总长度（prompt 与输出共享）
    pub context_length: Option<u32>,
    /// 网关直接给出的最大输出 token 数（只有少数网关提供）
    pub max_output_tokens: Option<u32>,
    /// 命中了哪个端点，只用于 --debug
    pub source: Option<&'static str>,
}

/// prompt 的粗略 token 估算。
///
/// 中文 1 字约 1~1.5 token（UTF-8 3 字节），英文约 4 字节 1 token；统一按
/// 3 字节 1 token 会**高估** prompt，正好把余量留出来，比精确分词更稳妥
/// （估低了会让 `prompt + max_tokens` 撑爆窗口）。再加一点消息封装固定开销。
fn estimate_prompt_tokens(prompt_bytes: usize) -> u32 {
    const FRAMING_OVERHEAD: u32 = 256;
    (prompt_bytes / 3) as u32 + FRAMING_OVERHEAD
}

/// 除 prompt 之外再留的余量；以及输出预算的地板值，避免算出 0 发过去。
const RESERVE_TOKENS: u32 = 512;
const MIN_OUTPUT_TOKENS: u32 = 256;

/// 决定这次请求真正带上的 `max_tokens`。
///
/// - `cap` 是 config.json 里显式写的值，语义是**上限**而不是固定值：模型窗口比它
///   小时按窗口算。过去「给推理模型配一个超大值兜底」的做法因此不再需要跟着模型
///   来回调整——换个窗口小的模型，预算自动收窄。
/// - 探测到最大输出：直接用它，但不超过窗口扣掉 prompt 后剩下的额度。
/// - 只探测到窗口：扣掉 prompt 估算与余量。
/// - 什么都没探测到：有 cap 就用 cap（兼容旧配置），否则 `None`（请求里省略字段）。
pub fn resolve_max_tokens(
    cap: Option<u32>,
    limits: ModelLimits,
    prompt_bytes: usize,
) -> Option<u32> {
    let consumed = estimate_prompt_tokens(prompt_bytes) + RESERVE_TOKENS;
    let window_budget = || {
        limits
            .context_length
            .map(|window| window.saturating_sub(consumed).max(MIN_OUTPUT_TOKENS))
    };

    let auto = match (limits.max_output_tokens, window_budget()) {
        (Some(max_output), Some(budget)) => Some(max_output.min(budget)),
        (Some(max_output), None) => Some(max_output),
        (None, budget) => budget,
    };

    match (auto, cap) {
        (Some(auto), Some(cap)) => Some(auto.min(cap)),
        (Some(auto), None) => Some(auto),
        (None, Some(cap)) => Some(cap),
        (None, None) => None,
    }
}

/// 去掉 `base_url` 末尾的 `/v1` 和 `/chat/completions`，得到站点根。
/// 用于拼 LM Studio / LiteLLM 那些不带 `/v1` 前缀的私有端点。
fn site_root(base_url: &str) -> String {
    let mut s = base_url.trim_end_matches('/');
    if let Some(stripped) = s.strip_suffix("/chat/completions") {
        s = stripped.trim_end_matches('/');
    }
    s.strip_suffix("/v1")
        .unwrap_or(s)
        .trim_end_matches('/')
        .to_string()
}

/// 标准模型列表地址：保留 `base_url` 里原有的 `/v1` 或 `/api/v1` 前缀。
fn models_endpoint(base_url: &str) -> String {
    let mut s = base_url.trim_end_matches('/');
    if let Some(stripped) = s.strip_suffix("/chat/completions") {
        s = stripped.trim_end_matches('/');
    }
    format!("{s}/models")
}

/// 按顺序尝试几个端点，第一个给出有效上限就返回。
///
/// 三个解析器对每个响应都会试一遍，按形状匹配而不是按端点分派：这样即便某个网关
/// 把私有端点挂在别的路径上，只要返回结构认得出来就能用。全部失败返回默认值，
/// 不向用户报错——这只是一次优化，不该让提交流程因为一个 404 中断。
pub async fn probe(config: &ModelConfig) -> ModelLimits {
    let Ok(client) = Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(3))
        .build()
    else {
        return ModelLimits::default();
    };

    let root = site_root(&config.base_url);
    let candidates: Vec<(&'static str, String)> = vec![
        ("lm-studio /api/v1/models", format!("{root}/api/v1/models")),
        ("models list", models_endpoint(&config.base_url)),
        ("litellm /model/info", format!("{root}/model/info")),
    ];

    for (source, url) in candidates {
        let Some(body) = fetch_json(&client, &url, &config.api_token).await else {
            continue;
        };
        let parsed = parse_lm_studio(&body, &config.model)
            .or_else(|| parse_models_list(&body, &config.model))
            .or_else(|| parse_litellm_info(&body, &config.model));

        if let Some(mut limits) = parsed {
            if limits.context_length.is_some() || limits.max_output_tokens.is_some() {
                limits.source = Some(source);
                if debug::enabled() {
                    eprintln!(
                        "[debug] 模型能力探测: {source} → context_length={:?} max_output_tokens={:?}",
                        limits.context_length, limits.max_output_tokens
                    );
                }
                return limits;
            }
        }
    }

    if debug::enabled() {
        eprintln!("[debug] 模型能力探测: 未拿到 token 上限，请求将省略 max_tokens");
    }
    ModelLimits::default()
}

async fn fetch_json(client: &Client, url: &str, token: &str) -> Option<Value> {
    let response = client
        .get(url)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json::<Value>().await.ok()
}

/// 解析 LM Studio 原生 `/api/v1/models`。
///
/// `loaded_instances[0].config.context_length` 是**实际加载时**设定的窗口，可能比模型的
/// `max_context_length` 小，优先用它才不会把预算算到用不上去的长度。
fn parse_lm_studio(body: &Value, model: &str) -> Option<ModelLimits> {
    let list = body.get("models")?.as_array()?;
    let entry = find_model(list, model)?;
    let loaded = u32_field(entry, "/loaded_instances/0/config/context_length");
    let context_length = loaded.or_else(|| u32_field(entry, "/max_context_length"));
    Some(ModelLimits {
        context_length,
        ..Default::default()
    })
}

/// 解析 OpenAI 兼容 `/v1/models` 里的各家扩展字段。
///
/// 标准实现只有 `id / object / owned_by`，这里能取到值全靠扩展：
/// OpenRouter 的 `top_provider.max_completion_tokens` / `context_length`、
/// vLLM 的 `max_model_len`、Anthropic 原生 `/v1/models` 的 `max_tokens`。
fn parse_models_list(body: &Value, model: &str) -> Option<ModelLimits> {
    let list = body.get("data")?.as_array()?;
    let entry = find_model(list, model)?;

    let max_output_tokens = u32_field(entry, "/top_provider/max_completion_tokens")
        .or_else(|| u32_field(entry, "/max_completion_tokens"))
        .or_else(|| u32_field(entry, "/max_output_tokens"));
    let context_length = u32_field(entry, "/context_length")
        .or_else(|| u32_field(entry, "/top_provider/context_length"))
        .or_else(|| u32_field(entry, "/max_model_len"))
        .or_else(|| u32_field(entry, "/max_tokens"))
        .or_else(|| u32_field(entry, "/max_input_tokens"));

    Some(ModelLimits {
        context_length,
        max_output_tokens,
        ..Default::default()
    })
}

/// 解析 LiteLLM `/model/info` 的 `data[].model_info`。
fn parse_litellm_info(body: &Value, model: &str) -> Option<ModelLimits> {
    let list = body.get("data")?.as_array()?;
    let entry = find_model(list, model)?;
    let info = entry.get("model_info")?;

    Some(ModelLimits {
        max_output_tokens: u32_field(info, "/max_output_tokens"),
        context_length: u32_field(info, "/max_tokens")
            .or_else(|| u32_field(info, "/max_input_tokens")),
        ..Default::default()
    })
}

fn u32_field(value: &Value, path: &str) -> Option<u32> {
    value
        .pointer(path)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
}

/// 找出目标模型对应的条目。先精确匹配，再放宽到「 provider/ 前缀」与变体后缀。
fn find_model<'a>(list: &'a [Value], model: &str) -> Option<&'a Value> {
    list.iter()
        .find(|entry| model_key(entry) == Some(model))
        .or_else(|| {
            list.iter().find(|entry| {
                model_key(entry)
                    .map(|key| loose_match(key, model))
                    .unwrap_or(false)
            })
        })
}

fn model_key(entry: &Value) -> Option<&str> {
    ["key", "id", "model_name"]
        .iter()
        .find_map(|name| entry.get(*name).and_then(Value::as_str))
        .or_else(|| {
            entry
                .pointer("/litellm_params/model")
                .and_then(Value::as_str)
        })
}

/// `qwen/qwen3-4b` 与 `qwen3-4b`、`foo@4bit` 与 `foo` 视为同一个模型。
fn loose_match(key: &str, model: &str) -> bool {
    let key = key.split('@').next().unwrap_or(key);
    let model = model.split('@').next().unwrap_or(model);
    key == model
        || key.split('/').next_back() == Some(model)
        || model.split('/').next_back() == Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(context_length: Option<u32>) -> ModelLimits {
        ModelLimits {
            context_length,
            ..Default::default()
        }
    }

    #[test]
    fn endpoint_urls_keep_the_original_v1_prefix() {
        assert_eq!(
            site_root("http://localhost:1234/v1"),
            "http://localhost:1234"
        );
        assert_eq!(
            models_endpoint("http://localhost:1234/v1"),
            "http://localhost:1234/v1/models"
        );
        assert_eq!(
            models_endpoint("https://openrouter.ai/api/v1"),
            "https://openrouter.ai/api/v1/models"
        );
        assert_eq!(
            models_endpoint("http://host:4000/v1/chat/completions"),
            "http://host:4000/v1/models"
        );
        assert_eq!(site_root("http://host:4000/v1/"), "http://host:4000");
    }

    /// 核心场景：配置里那个「给推理模型兜底」的超大值，遇到窗口小的模型必须自动收窄。
    #[test]
    fn configured_cap_is_clamped_by_the_model_window() {
        let small = resolve_max_tokens(Some(409_600), limits(Some(32_768)), 3_000);
        let large = resolve_max_tokens(Some(409_600), limits(Some(262_144)), 3_000);

        assert!(small.unwrap() < 32_768, "{small:?}");
        assert!(large.unwrap() > small.unwrap(), "{large:?} / {small:?}");
        assert!(large.unwrap() < 262_144, "{large:?}");
    }

    /// 用户显式设一个小值仍然生效（想限制输出长度时不受探测影响）
    #[test]
    fn explicit_small_cap_still_wins() {
        assert_eq!(
            resolve_max_tokens(Some(512), limits(Some(262_144)), 3_000),
            Some(512)
        );
    }

    /// 探测不到时兼容旧行为：有配置用配置；彻底没有配置就省略字段
    #[test]
    fn falls_back_to_cap_then_to_none() {
        let unknown = ModelLimits::default();
        assert_eq!(resolve_max_tokens(Some(4096), unknown, 3_000), Some(4096));
        assert_eq!(resolve_max_tokens(None, unknown, 3_000), None);
    }

    /// 网关直接给出最大输出时，仍不能超出窗口扣掉 prompt 后的额度
    #[test]
    fn max_output_is_bounded_by_the_window() {
        let limits = ModelLimits {
            context_length: Some(32_768),
            max_output_tokens: Some(1_000_000),
            ..Default::default()
        };
        assert!(resolve_max_tokens(None, limits, 3_000).unwrap() < 32_768);
    }

    /// prompt 越大，留给输出的预算越小
    #[test]
    fn larger_prompt_leaves_a_smaller_budget() {
        let short = resolve_max_tokens(None, limits(Some(32_768)), 300).unwrap();
        let long = resolve_max_tokens(None, limits(Some(32_768)), 60_000).unwrap();
        assert!(long < short, "{long} / {short}");
    }

    #[test]
    fn parses_lm_studio_and_prefers_the_loaded_context_length() {
        let body: Value = serde_json::from_str(
            r#"{"models":[
                {"key":"qwen/qwen3-4b-thinking-2507","max_context_length":262144,
                 "loaded_instances":[]},
                {"key":"qwen2.5-coder-7b-instruct-mlx","max_context_length":32768,
                 "loaded_instances":[{"id":"x","config":{"context_length":16384}}]}
            ]}"#,
        )
        .unwrap();

        let parsed = parse_lm_studio(&body, "qwen2.5-coder-7b-instruct-mlx").unwrap();
        assert_eq!(parsed.context_length, Some(16_384));
        assert_eq!(
            parse_lm_studio(&body, "qwen/qwen3-4b-thinking-2507")
                .unwrap()
                .context_length,
            Some(262_144)
        );
    }

    #[test]
    fn parses_openrouter_style_model_list() {
        let body: Value = serde_json::from_str(
            r#"{"data":[{"id":"x/model","context_length":128000,
                "top_provider":{"context_length":128000,"max_completion_tokens":32000}}]}"#,
        )
        .unwrap();

        let parsed = parse_models_list(&body, "x/model").unwrap();
        assert_eq!(parsed.context_length, Some(128_000));
        assert_eq!(parsed.max_output_tokens, Some(32_000));
    }

    /// 标准 `/v1/models` 没有任何扩展字段时，解析结果为空，调用方会继续试下一个端点
    #[test]
    fn plain_openai_models_list_yields_no_limits() {
        let body: Value = serde_json::from_str(
            r#"{"data":[{"id":"qwen2.5","object":"model","owned_by":"organization_owner"}]}"#,
        )
        .unwrap();

        assert_eq!(
            parse_models_list(&body, "qwen2.5").unwrap(),
            ModelLimits::default()
        );
    }

    #[test]
    fn parses_vllm_and_litellm_shapes() {
        let vllm: Value =
            serde_json::from_str(r#"{"data":[{"id":"local-model","max_model_len":65536}]}"#)
                .unwrap();
        assert_eq!(
            parse_models_list(&vllm, "local-model")
                .unwrap()
                .context_length,
            Some(65_536)
        );

        let litellm: Value = serde_json::from_str(
            r#"{"data":[{"model_name":"alias","litellm_params":{"model":"openai/gpt-4o"},
                "model_info":{"max_tokens":128000,"max_output_tokens":16384}}]}"#,
        )
        .unwrap();
        let parsed = parse_litellm_info(&litellm, "alias").unwrap();
        assert_eq!(parsed.context_length, Some(128_000));
        assert_eq!(parsed.max_output_tokens, Some(16_384));
    }

    #[test]
    fn model_name_matching_tolerates_provider_prefix_and_variants() {
        let list: Vec<Value> = vec![serde_json::json!({"key": "qwen/qwen3-4b@4bit"})];
        assert!(find_model(&list, "qwen3-4b").is_some());
        assert!(find_model(&list, "qwen/qwen3-4b").is_some());
        assert!(find_model(&list, "unrelated").is_none());
    }
}
