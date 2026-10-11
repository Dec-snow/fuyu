//! OpenAI Chat Completions 适配器。
//!
//! 只做两件事：
//! 1. 把内部消息模型**构造**成请求体；
//! 2. 把厂商返回的 JSON **归一化**成 [`ParsedChunk`]。
//!
//! 它完全不碰字节流和换行 —— 那是 [`super::sse::SseParser`] 的活。
//! 换一家厂商（智谱 / Kimi / 通义 / Ollama / OpenRouter…）通常只要改这个
//! 文件里的字段兼容分支，上层和前端一行都不用动。

use serde_json::{json, Value};

use crate::errors::{code, AppError, AppResult};
use crate::models::chat::ChatMessage;
use crate::storage::config::normalize_base_url;

/// 把用户填的 Base URL 规范化并拼出 `chat/completions` 端点。
pub fn chat_completions_url(base_url: &str) -> AppResult<String> {
    let base = normalize_base_url(base_url)?;
    Ok(format!("{base}/chat/completions"))
}

/// 把 Base URL 规范化并拼出 `models` 端点（用于「拉取模型列表」）。
pub fn models_url(base_url: &str) -> AppResult<String> {
    let base = normalize_base_url(base_url)?;
    Ok(format!("{base}/models"))
}

/// 内部消息 → OpenAI `messages` 数组。
///
/// `system_prompt` 非空时插到最前面。这里刻意**不**做角色合并、不丢弃历史：
/// 上下文裁剪是上层（`agent/context`）的职责，适配器保持“所见即所得”，
/// 方便单测。
///
/// 三种特殊形态（agent 工具循环用）：
/// - `assistant` + `tool_calls` → `content: null` + `tool_calls` 数组；
/// - `tool` → `tool_call_id` + `content`（工具结果回灌）；
/// - 其余 → 普通 `role` + `content`，未知角色一律当 `user`。
pub fn messages_to_openai(messages: &[ChatMessage], system_prompt: Option<&str>) -> Vec<Value> {
    let mut out = Vec::with_capacity(messages.len() + 1);

    if let Some(prompt) = system_prompt {
        if !prompt.trim().is_empty() {
            out.push(json!({ "role": "system", "content": prompt }));
        }
    }

    for m in messages {
        if m.role == "assistant" {
            if let Some(tool_calls) = &m.tool_calls {
                let calls: Vec<Value> = tool_calls
                    .iter()
                    .map(|call| {
                        json!({
                            "id": call.id,
                            "type": "function",
                            "function": { "name": call.name, "arguments": call.arguments },
                        })
                    })
                    .collect();
                // 有正文就把正文带上（模型调用工具前常说的那句），
                // 没有才写 `null` —— 规范允许 `content` 为 null，
                // 但不能把一个空串当成「它什么都没说」。
                let content = if m.content.trim().is_empty() {
                    Value::Null
                } else {
                    Value::String(m.content.clone())
                };
                out.push(json!({
                    "role": "assistant",
                    "content": content,
                    "tool_calls": calls,
                }));
                continue;
            }
        }

        if m.role == "tool" {
            out.push(json!({
                "role": "tool",
                "tool_call_id": m.tool_call_id,
                "content": m.content,
            }));
            continue;
        }

        // 带图的消息要用**数组**形态的 content：
        //   {"role":"user","content":[{"type":"text",...},{"type":"image_url",...}]}
        // 注意顺序：文字在前、图片在后（OpenAI 与国内几家兼容实现都按这个顺序
        // 处理；反过来有些服务端会报参数错误）。
        if let Some(images) = &m.images {
            if !images.is_empty() {
                let mut parts: Vec<Value> = Vec::with_capacity(images.len() + 1);
                if !m.content.is_empty() {
                    parts.push(json!({ "type": "text", "text": m.content }));
                }
                for url in images {
                    parts.push(json!({
                        "type": "image_url",
                        "image_url": { "url": url },
                    }));
                }
                out.push(json!({ "role": "user", "content": parts }));
                continue;
            }
        }

        let role = match m.role.as_str() {
            "assistant" | "system" | "user" => m.role.as_str(),
            // 未知角色一律当 user，避免服务端直接 400。
            _ => "user",
        };
        out.push(json!({ "role": role, "content": m.content }));
    }
    out
}

/// 构造非流式请求体（滚动摘要用）。
pub fn build_completion_body(
    model: &str,
    messages: &[ChatMessage],
    system_prompt: Option<&str>,
    max_tokens: Option<u32>,
) -> Value {
    let mut body = json!({
        "model": model,
        "messages": messages_to_openai(messages, system_prompt),
        "stream": false,
    });
    if let Some(m) = max_tokens {
        body["max_tokens"] = json!(m);
    }
    body
}

/// 构造流式请求体。
///
/// `tools` 传入 [`crate::tools::registry::to_openai_tools`] 的结果时，模型就
/// 可以在流里返回 `tool_calls`；传 `None` 则是不带工具能力的普通问答。
pub fn build_stream_body(
    model: &str,
    messages: &[ChatMessage],
    system_prompt: Option<&str>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    tools: Option<&Value>,
) -> Value {
    let mut body = json!({
        "model": model,
        "messages": messages_to_openai(messages, system_prompt),
        "stream": true,
        // 让服务端在流的最后补一个只带 usage 的 chunk。
        // 不写这一句，很多中转就永远不给 token 用量。
        // 代价：极少数严格校验字段的服务端会 400 —— 遇到时把它删掉即可。
        "stream_options": { "include_usage": true },
    });

    if let Some(t) = temperature {
        body["temperature"] = json!(t);
    }
    if let Some(m) = max_tokens {
        body["max_tokens"] = json!(m);
    }
    if let Some(tools) = tools {
        body["tools"] = tools.clone();
    }
    body
}

// ------------------------------------------------------------------ 响应解析

#[derive(Debug, Clone, PartialEq)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

/// 一个 chunk 里可能同时出现的内容片段。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChunkParts {
    pub content: Option<String>,
    pub reasoning: Option<String>,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
    /// 原生 function calling 的增量：参数是**分片**到达的，需要在流式消费层
    /// 按 `index` 累积。
    pub tool_call_deltas: Vec<ToolCallDelta>,
}

/// 一个 tool_call 增量。`id` / `name` 只在第一片出现，`arguments` 逐片累积。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ToolCallDelta {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParsedChunk {
    /// 没有可用字段（例如只有 role 的首片），忽略即可。
    Empty,
    /// `[DONE]` 哨兵。
    Done,
    /// 服务端把错误塞进了流里（有些中转 HTTP 200 也这么干）。
    Error {
        message: String,
        retryable: bool,
    },
    /// 不是合法 JSON，或结构不认识。跳过，不要中断整条流。
    Malformed,
    Parts(ChunkParts),
}

/// 解析一个 SSE event 的 `data:` payload。
pub fn parse_stream_chunk(data: &str) -> ParsedChunk {
    let trimmed = data.trim();
    if trimmed.is_empty() {
        return ParsedChunk::Empty;
    }
    if trimmed == "[DONE]" {
        return ParsedChunk::Done;
    }

    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return ParsedChunk::Malformed;
    };

    // 有些中转把错误当正常事件发：{"error":{"message":"..."}}
    if let Some(err) = value.get("error") {
        if !err.is_null() {
            let message = err
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| err.as_str())
                .unwrap_or("服务端返回了未说明的错误")
                .to_string();
            return ParsedChunk::Error {
                message,
                retryable: true,
            };
        }
    }

    let mut parts = ChunkParts::default();

    if let Some(choice) = value.get("choices").and_then(|c| c.get(0)) {
        if let Some(delta) = choice.get("delta") {
            parts.content = content_value_to_text(delta.get("content"));
            parts.reasoning = delta
                .get("reasoning_content")
                .and_then(Value::as_str)
                // OpenRouter 用 `reasoning`，部分中转用 `thinking`。
                .or_else(|| delta.get("reasoning").and_then(Value::as_str))
                .or_else(|| delta.get("thinking").and_then(Value::as_str))
                .map(str::to_string);
            if parts.reasoning.as_deref() == Some("") {
                parts.reasoning = None;
            }

            // 原生 function calling：delta.tool_calls 是分片到达的数组。
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    let Some(index) = call.get("index").and_then(Value::as_u64) else {
                        continue;
                    };
                    let function = call.get("function");
                    parts.tool_call_deltas.push(ToolCallDelta {
                        index: index as usize,
                        id: call.get("id").and_then(Value::as_str).map(str::to_string),
                        name: function
                            .and_then(|f| f.get("name"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        arguments: function
                            .and_then(|f| f.get("arguments"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    });
                }
            }
        }
        // 非流式结构偶尔混进来（某些中转的最后一帧）。
        if parts.content.is_none() {
            parts.content = choice
                .get("message")
                .and_then(|m| content_value_to_text(m.get("content")));
        }
        // 兼容旧的 /completions 结构。
        if parts.content.is_none() {
            parts.content = choice
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string);
        }

        // 关键：空字符串的 finish_reason 必须当作「还没结束」。
        // 有的服务端每一帧都带 `"finish_reason": ""`，若当成结束，
        // 用户就只会看到一个字的回答。
        parts.finish_reason = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }

    if let Some(usage) = value.get("usage").filter(|u| !u.is_null()) {
        let input = usage_number(usage, &["prompt_tokens", "input_tokens"]);
        let output = usage_number(usage, &["completion_tokens", "output_tokens"]);
        if input.is_some() || output.is_some() {
            parts.usage = Some(Usage {
                input_tokens: input,
                output_tokens: output,
            });
        }
    }

    if parts == ChunkParts::default() {
        ParsedChunk::Empty
    } else {
        ParsedChunk::Parts(parts)
    }
}

fn usage_number(usage: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|k| usage.get(*k).and_then(Value::as_u64))
}

/// `content` 可能是字符串，也可能是多模态分片数组。
fn content_value_to_text(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(s) => {
            if s.is_empty() {
                None
            } else {
                Some(s.clone())
            }
        }
        Value::Array(items) => {
            let mut out = String::new();
            for item in items {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    out.push_str(text);
                }
            }
            if out.is_empty() {
                None
            } else {
                Some(out)
            }
        }
        _ => None,
    }
}

/// HTTP 状态码 → 结构化错误。返回的 `retryable` 决定前端是否给「重试」。
pub fn map_http_error(status: u16, body: &str) -> AppError {
    let detail = extract_error_detail(body);
    match status {
        401 | 403 => AppError::new(
            code::UNAUTHORIZED,
            format!("鉴权失败（{status}）：{detail}"),
        ),
        404 => AppError::new(
            code::NOT_FOUND,
            format!("接口不存在（404）：请检查 Base URL 是否填成了完整端点；{detail}"),
        ),
        429 => AppError::new(code::RATE_LIMITED, format!("请求过于频繁（429）：{detail}"))
            .retryable(true),
        400 | 422 => AppError::new(
            code::INVALID_INPUT,
            format!("请求被拒绝（{status}）：{detail}"),
        ),
        500..=599 => {
            AppError::new(code::SERVER, format!("服务端错误（{status}）：{detail}")).retryable(true)
        }
        _ => AppError::new(code::SERVER, format!("请求失败（{status}）：{detail}")),
    }
}

/// 从各家五花八门的错误体里抠出一句人能看的话。
fn extract_error_detail(body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        if let Some(msg) = value
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .or_else(|| value.get("message").and_then(Value::as_str))
            .or_else(|| value.get("error").and_then(Value::as_str))
            .or_else(|| value.get("detail").and_then(Value::as_str))
        {
            return truncate(msg, 300);
        }
    }
    let fallback = body.trim();
    if fallback.is_empty() {
        "服务端未返回错误详情".to_string()
    } else {
        truncate(fallback, 300)
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// 非流式响应里取正文（「测试连接」用）。
pub fn extract_completion_text(body: &str) -> AppResult<String> {
    let value: Value = serde_json::from_str(body)
        .map_err(|_| AppError::new(code::SERVER, "服务端返回的不是合法 JSON"))?;
    if let Some(err) = value.get("error").filter(|e| !e.is_null()) {
        return Err(AppError::new(
            code::SERVER,
            err.get("message")
                .and_then(Value::as_str)
                .unwrap_or("服务端返回错误"),
        ));
    }
    value
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| {
            content_value_to_text(c.get("message").and_then(|m| m.get("content")))
                .or_else(|| content_value_to_text(c.get("text")))
        })
        .ok_or_else(|| AppError::new(code::SERVER, "响应里没有可用的正文"))
}

/// `GET /models` 的结果 → 模型 ID 列表。
pub fn parse_models_response(body: &str) -> AppResult<Vec<String>> {
    let value: Value = serde_json::from_str(body)
        .map_err(|_| AppError::new(code::SERVER, "模型列表不是合法 JSON"))?;
    let list = value
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| value.get("models").and_then(Value::as_array))
        .or_else(|| value.as_array())
        .ok_or_else(|| AppError::new(code::SERVER, "响应里没有模型列表"))?;

    let mut out: Vec<String> = list
        .iter()
        .filter_map(|item| {
            item.get("id")
                .or_else(|| item.get("name"))
                .and_then(Value::as_str)
                .or_else(|| item.as_str())
                .map(str::to_string)
        })
        .collect();
    out.sort();
    out.dedup();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn url_join_handles_trailing_slash_and_full_endpoint() {
        assert_eq!(
            chat_completions_url("https://api.deepseek.com/v1").unwrap(),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://api.deepseek.com/v1/").unwrap(),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://api.deepseek.com/v1/chat/completions").unwrap(),
            "https://api.deepseek.com/v1/chat/completions"
        );
    }

    #[test]
    fn system_prompt_is_prepended_only_when_present() {
        let messages = vec![msg("user", "hi")];
        let with = messages_to_openai(&messages, Some("你是助手"));
        assert_eq!(with.len(), 2);
        assert_eq!(with[0]["role"], "system");

        let without = messages_to_openai(&messages, Some("   "));
        assert_eq!(without.len(), 1);
        let none = messages_to_openai(&messages, None);
        assert_eq!(none.len(), 1);
    }

    #[test]
    fn messages_without_images_keep_the_plain_string_form() {
        let out = messages_to_openai(&[msg("user", "你好")], None);
        assert!(
            out[0]["content"].is_string(),
            "无图消息要保持字符串形态 —— 兼容性最好，也少一层解析"
        );
    }

    #[test]
    fn user_messages_with_images_use_the_array_content_form() {
        let message = ChatMessage {
            role: "user".to_string(),
            content: "这是什么？".to_string(),
            images: Some(vec![
                "data:image/png;base64,AAAA".to_string(),
                "data:image/jpeg;base64,BBBB".to_string(),
            ]),
            ..Default::default()
        };

        let out = messages_to_openai(&[message], None);

        let parts = out[0]["content"].as_array().expect("content 应当是数组");
        assert_eq!(parts.len(), 3, "1 段文字 + 2 张图");
        // 顺序：文字在前、图片在后。
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["text"], "这是什么？");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,AAAA");
        assert_eq!(parts[2]["image_url"]["url"], "data:image/jpeg;base64,BBBB");
    }

    #[test]
    fn image_only_messages_omit_the_empty_text_part() {
        let message = ChatMessage {
            role: "user".to_string(),
            content: String::new(),
            images: Some(vec!["data:image/png;base64,AAAA".to_string()]),
            ..Default::default()
        };

        let out = messages_to_openai(&[message], None);
        let parts = out[0]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 1, "没有文字时不该塞一个空的 text 段");
        assert_eq!(parts[0]["type"], "image_url");
    }

    #[test]
    fn assistant_text_before_a_tool_call_is_preserved() {
        // 模型常先说明一句再调工具。那句话必须一起带回历史，
        // 否则后续轮次里模型看不到自己说过什么。
        let message = ChatMessage {
            role: "assistant".to_string(),
            content: "我先看一下你的剪贴板。".to_string(),
            tool_calls: Some(vec![crate::models::chat::ToolCall {
                id: "call_1".to_string(),
                name: "read_clipboard_text".to_string(),
                arguments: "{}".to_string(),
            }]),
            ..Default::default()
        };

        let out = messages_to_openai(&[message], None);

        assert_eq!(
            out[0]["content"], "我先看一下你的剪贴板。",
            "调用工具前的说明必须留在 assistant 消息里"
        );
        assert_eq!(out[0]["tool_calls"][0]["id"], "call_1");
    }

    #[test]
    fn an_assistant_tool_call_without_text_uses_null_content() {
        let message = ChatMessage {
            role: "assistant".to_string(),
            content: "   ".to_string(),
            tool_calls: Some(vec![crate::models::chat::ToolCall {
                id: "call_1".to_string(),
                name: "calculator".to_string(),
                arguments: "{}".to_string(),
            }]),
            ..Default::default()
        };

        let out = messages_to_openai(&[message], None);
        assert!(
            out[0]["content"].is_null(),
            "没有正文时按规范写 null，而不是空串"
        );
    }

    #[test]
    fn unknown_role_degrades_to_user() {
        let out = messages_to_openai(&[msg("foo", "x")], None);
        assert_eq!(out[0]["role"], "user");
    }

    #[test]
    fn assistant_tool_calls_and_tool_results_map_to_openai_shape() {
        use crate::models::chat::ToolCall;

        let assistant = ChatMessage {
            role: "assistant".to_string(),
            content: String::new(),
            tool_calls: Some(vec![ToolCall {
                id: "call_1".to_string(),
                name: "calculator".to_string(),
                arguments: "{\"expression\":\"1+1\"}".to_string(),
            }]),
            ..Default::default()
        };
        let tool = ChatMessage {
            role: "tool".to_string(),
            content: "2".to_string(),
            tool_call_id: Some("call_1".to_string()),
            ..Default::default()
        };

        let out = messages_to_openai(&[assistant, tool], None);

        // assistant 消息：content 为 null，tool_calls 按规范拼装。
        assert_eq!(out[0]["role"], "assistant");
        assert!(out[0]["content"].is_null());
        assert_eq!(out[0]["tool_calls"][0]["id"], "call_1");
        assert_eq!(out[0]["tool_calls"][0]["function"]["name"], "calculator");

        // tool 消息：tool_call_id + content 回灌。
        assert_eq!(out[1]["role"], "tool");
        assert_eq!(out[1]["tool_call_id"], "call_1");
        assert_eq!(out[1]["content"], "2");
    }

    #[test]
    fn stream_body_carries_usage_option_and_params() {
        let body = build_stream_body("m", &[msg("user", "q")], None, Some(0.2), Some(128), None);
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["max_tokens"], 128);

        // f32 → f64 会带出精度尾巴（0.2f32 as f64 = 0.20000000298023224），
        // 所以这里比较近似值而不是精确相等。
        let temperature = body["temperature"].as_f64().expect("temperature");
        assert!(
            (temperature - 0.2).abs() < 1e-6,
            "temperature = {temperature}"
        );

        // 未提供的参数不应该出现在请求体里（而不是写成 null）。
        let bare = build_stream_body("m", &[msg("user", "q")], None, None, None, None);
        assert!(bare.get("temperature").is_none());
        assert!(bare.get("max_tokens").is_none());
        assert!(bare.get("tools").is_none());
    }

    #[test]
    fn done_sentinel_is_recognised() {
        assert_eq!(parse_stream_chunk("[DONE]"), ParsedChunk::Done);
        assert_eq!(parse_stream_chunk("  [DONE]  "), ParsedChunk::Done);
    }

    #[test]
    fn delta_content_is_extracted() {
        let data = r#"{"choices":[{"delta":{"content":"你好"},"index":0}]}"#;
        match parse_stream_chunk(data) {
            ParsedChunk::Parts(p) => assert_eq!(p.content.as_deref(), Some("你好")),
            other => panic!("expected parts, got {other:?}"),
        }
    }

    #[test]
    fn empty_finish_reason_means_not_finished() {
        // 这是「回答只出一个字」那个经典 bug 的守门测试。
        let data = r#"{"choices":[{"delta":{"content":"一"},"finish_reason":""}]}"#;
        match parse_stream_chunk(data) {
            ParsedChunk::Parts(p) => {
                assert_eq!(p.content.as_deref(), Some("一"));
                assert_eq!(p.finish_reason, None, "空串必须当作“未结束”");
            }
            other => panic!("expected parts, got {other:?}"),
        }
    }

    #[test]
    fn real_finish_reason_is_kept() {
        let data = r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#;
        match parse_stream_chunk(data) {
            ParsedChunk::Parts(p) => assert_eq!(p.finish_reason.as_deref(), Some("stop")),
            other => panic!("expected parts, got {other:?}"),
        }
    }

    #[test]
    fn reasoning_field_variants_are_supported() {
        for key in ["reasoning_content", "reasoning", "thinking"] {
            let data = format!(r#"{{"choices":[{{"delta":{{"{key}":"想一想"}}}}]}}"#);
            match parse_stream_chunk(&data) {
                ParsedChunk::Parts(p) => assert_eq!(p.reasoning.as_deref(), Some("想一想")),
                other => panic!("{key} 未识别：{other:?}"),
            }
        }
    }

    #[test]
    fn usage_is_read_from_either_naming() {
        let openai = r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":20}}"#;
        match parse_stream_chunk(openai) {
            ParsedChunk::Parts(p) => {
                let u = p.usage.expect("usage");
                assert_eq!((u.input_tokens, u.output_tokens), (Some(10), Some(20)));
            }
            other => panic!("expected parts, got {other:?}"),
        }

        let alt = r#"{"choices":[],"usage":{"input_tokens":1,"output_tokens":2}}"#;
        match parse_stream_chunk(alt) {
            ParsedChunk::Parts(p) => {
                let u = p.usage.expect("usage");
                assert_eq!((u.input_tokens, u.output_tokens), (Some(1), Some(2)));
            }
            other => panic!("expected parts, got {other:?}"),
        }
    }

    #[test]
    fn role_only_first_chunk_is_empty() {
        let data = r#"{"choices":[{"delta":{"role":"assistant"},"index":0}]}"#;
        assert_eq!(parse_stream_chunk(data), ParsedChunk::Empty);
    }

    #[test]
    fn malformed_json_does_not_panic() {
        assert_eq!(parse_stream_chunk("{not json"), ParsedChunk::Malformed);
        assert_eq!(parse_stream_chunk(""), ParsedChunk::Empty);
    }

    #[test]
    fn in_stream_error_is_surfaced() {
        let data = r#"{"error":{"message":"余额不足"}}"#;
        match parse_stream_chunk(data) {
            ParsedChunk::Error { message, .. } => assert!(message.contains("余额不足")),
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn content_array_is_flattened() {
        let data = r#"{"choices":[{"delta":{"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}}]}"#;
        match parse_stream_chunk(data) {
            ParsedChunk::Parts(p) => assert_eq!(p.content.as_deref(), Some("ab")),
            other => panic!("expected parts, got {other:?}"),
        }
    }

    #[test]
    fn http_errors_are_classified() {
        assert_eq!(map_http_error(401, "{}").code, code::UNAUTHORIZED);
        assert!(!map_http_error(401, "{}").retryable);
        assert!(map_http_error(429, "{}").retryable);
        assert!(map_http_error(503, "{}").retryable);
        assert_eq!(map_http_error(400, "{}").code, code::INVALID_INPUT);
    }

    #[test]
    fn error_detail_is_extracted_from_vendor_shapes() {
        assert_eq!(
            extract_error_detail(r#"{"error":{"message":"额度不足"}}"#),
            "额度不足"
        );
        assert_eq!(extract_error_detail(r#"{"message":"坏了"}"#), "坏了");
        assert_eq!(extract_error_detail("plain text"), "plain text");
        assert_eq!(extract_error_detail("   "), "服务端未返回错误详情");
    }

    #[test]
    fn models_list_is_parsed_and_sorted() {
        let body = r#"{"data":[{"id":"b"},{"id":"a"},{"id":"a"}]}"#;
        assert_eq!(parse_models_response(body).unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn completion_text_is_extracted() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"好的"}}]}"#;
        assert_eq!(extract_completion_text(body).unwrap(), "好的");
    }
}
