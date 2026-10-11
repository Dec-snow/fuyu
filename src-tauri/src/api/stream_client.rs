//! 流式请求的**可测试核心**。
//!
//! 为什么把它从 `commands/chat.rs` 里拆出来：原来的实现直接持有 `AppHandle`
//! 和 `Database`，而这正是 Tauri 运行时里最难在测试中构造的两个东西。
//! 拆开之后这里只依赖「一个 HTTP 响应」和「一个回调」，于是可以对着一个
//! 本地假 SSE 服务端把整条链路跑通（见文件末尾的测试）。
//!
//! 分层：
//! ```text
//! send_with_retry  ── HTTP 语义（重试、Retry-After、状态码分类）
//! consume_stream   ── 流式语义（SSE 拆包 → 归一化 → 合批 → 回调）
//! commands::chat   ── Tauri 管道（emit 到前端、落库、首轮自动命名）
//! ```

use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::api::sse::SseParser;
use crate::api::{openai, MAX_ERROR_BODY};
use crate::errors::{code, read_body_capped, AppError, AppResult};
use crate::models::chat::{ChatEvent, ToolCall};

/// 「发送前」阶段的重试次数（建连 + 拿到 2xx 之前）。
pub const RETRY_MAX_ATTEMPTS: usize = 3;
/// 拿到 `finish_reason` 之后，再等多久看服务端会不会补 usage。
pub const USAGE_GRACE_MS: u64 = 1500;
/// 退避封顶。服务端给的 `Retry-After` 也受这个上限约束。
const MAX_BACKOFF_MS: u64 = 10_000;

/// 一次流式消费的结果。终态事件（Finish / Cancelled / Error）由调用方发 ——
/// 那样可以保证「先落库、再通知前端结束」，前端拿到 Finish 时历史已经写好了。
#[derive(Debug, Default, Clone)]
pub struct StreamOutcome {
    /// 累积的正文。
    pub content: String,
    /// 累积的思考过程。
    pub reasoning: String,
    pub finish_reason: Option<String>,
    pub usage: Option<openai::Usage>,
    /// 被取消（用户点了停止）。
    pub cancelled: bool,
    /// 出错。`content` 里可能仍有半截回答。
    pub error: Option<AppError>,
    /// 原生 function calling 累积出的工具调用。`finish_reason == "tool_calls"`
    /// 时这里非空，调用方应当执行工具并把结果回灌后再问一轮。
    pub tool_calls: Vec<ToolCall>,
}

impl StreamOutcome {
    /// 有没有值得写进历史的内容。
    pub fn has_content(&self) -> bool {
        !self.content.trim().is_empty()
    }
}

// ------------------------------------------------------------------ 发送

/// 发送请求；**只在流开始之前**重试。
///
/// 为什么不能在整个流上重试：一旦已经有 delta 推给前端，重试就会把同一段
/// 回答再拼一次，用户看到「你好你好」。所以 `Ok(response)` 一旦返回，
/// 重试就结束了。
///
/// 建连阶段也监听取消令牌：用户在「正在连接…」时点停止，不应该还要等
/// 十秒的 `connect_timeout` 才生效。
pub async fn send_with_retry(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    body: &Value,
    token: &CancellationToken,
) -> AppResult<reqwest::Response> {
    let mut last_error: Option<AppError> = None;

    for attempt in 1..=RETRY_MAX_ATTEMPTS {
        if token.is_cancelled() {
            return Err(AppError::new(code::CANCELLED, "已取消"));
        }

        let mut request = client
            .post(url)
            .header("Content-Type", "application/json")
            .json(body);
        if !api_key.is_empty() {
            request = request.bearer_auth(api_key);
        }

        let send = request.send();
        let outcome = tokio::select! {
            _ = token.cancelled() => return Err(AppError::new(code::CANCELLED, "已取消")),
            result = send => result,
        };

        match outcome {
            Ok(resp) if resp.status().is_success() => return Ok(resp),
            Ok(resp) => {
                let status = resp.status().as_u16();
                // `Retry-After` 必须在消费 body **之前**读 —— body 一读完，
                // 响应头就跟着一起没了。
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<u64>().ok());

                let text = read_body_capped(resp, MAX_ERROR_BODY)
                    .await
                    .unwrap_or_default();
                let err = openai::map_http_error(status, &text);

                if !err.retryable || attempt == RETRY_MAX_ATTEMPTS {
                    return Err(err);
                }
                last_error = Some(err);
                backoff_sleep(attempt, retry_after, token).await;
            }
            Err(e) => {
                let err = AppError::from(e);
                if !err.retryable || attempt == RETRY_MAX_ATTEMPTS {
                    return Err(err);
                }
                last_error = Some(err);
                backoff_sleep(attempt, None, token).await;
            }
        }
    }

    Err(last_error.unwrap_or_else(|| AppError::network("请求失败")))
}

/// 指数退避 + 抖动；服务端给了 `Retry-After` 就优先听它的。
///
/// 抖动很重要：一堆客户端同时被 429 之后，如果都按同一个节奏重试，
/// 会在同一时刻再次打爆服务端（惊群）。
///
/// 退避期间同样可被取消 —— 否则用户点停止后要干等好几秒。
async fn backoff_sleep(attempt: usize, retry_after: Option<u64>, token: &CancellationToken) {
    let ms = match retry_after {
        // 注意单位是秒，且要封顶，否则一个坏服务端能让我们睡到天荒地老。
        Some(secs) => secs.saturating_mul(1000).min(MAX_BACKOFF_MS),
        None => {
            let base = 500u64.saturating_mul(1 << (attempt.saturating_sub(1)).min(4));
            let jitter = (crate::storage::now_ms().unsigned_abs()) % 250;
            (base + jitter).min(MAX_BACKOFF_MS)
        }
    };

    tokio::select! {
        _ = token.cancelled() => {}
        _ = tokio::time::sleep(Duration::from_millis(ms)) => {}
    }
}

// ------------------------------------------------------------------ 消费

/// 消费一个流式响应，把归一化后的事件交给 `sink`。
///
/// 负责发出：`Delta` / `ReasoningDelta` / `Usage`。
/// **不**负责：`Start`（要在发请求之前就发）、`Finish` / `Cancelled` / `Error`
/// （终态，由调用方在落库之后发）。
pub async fn consume_stream<F>(
    response: reqwest::Response,
    request_id: &str,
    token: &CancellationToken,
    mut sink: F,
) -> StreamOutcome
where
    F: FnMut(ChatEvent),
{
    let mut outcome = StreamOutcome::default();
    let mut parser = SseParser::new();
    let mut stream = response.bytes_stream();

    let mut finished = false;
    let mut stop = false;

    // 原生 function calling：参数按 `index` 分片到达，用 BTreeMap 累积，
    // 流结束时再按 index 顺序组装成完整的 ToolCall。
    let mut tool_acc: std::collections::BTreeMap<usize, (Option<String>, Option<String>, String)> =
        std::collections::BTreeMap::new();

    while !stop {
        // 已经拿到结束原因和用量，没必要再等。
        if finished && outcome.usage.is_some() {
            break;
        }

        // 「等下一片数据」和「等取消信号」同时等，谁先到算谁。
        // 已结束时只再宽限一小段时间等 usage；否则一直读下去。
        let next = if finished {
            tokio::select! {
                _ = token.cancelled() => { outcome.cancelled = true; None }
                item = tokio::time::timeout(
                    Duration::from_millis(USAGE_GRACE_MS),
                    stream.next(),
                ) => item.unwrap_or(None), // 宽限期到，视为服务端不会给 usage
            }
        } else {
            tokio::select! {
                _ = token.cancelled() => { outcome.cancelled = true; None }
                item = stream.next() => item,
            }
        };

        if outcome.cancelled {
            break;
        }

        let Some(chunk) = next else {
            break;
        };

        let bytes = match chunk {
            Ok(bytes) => bytes,
            Err(e) => {
                // 流中途断掉：此时**不能重试**，否则会把已经显示的回答再拼一遍。
                let err = AppError::from(e);
                outcome.error = Some(AppError::new(
                    &err.code,
                    format!("流式连接中断：{}", err.message),
                ));
                break;
            }
        };

        // 一个网络分片里可能有好几帧，先合批再交给 sink，减少 IPC 次数。
        let mut pending_text = String::new();
        let mut pending_reasoning = String::new();

        for payload in parser.push(&bytes) {
            match openai::parse_stream_chunk(&payload) {
                openai::ParsedChunk::Empty | openai::ParsedChunk::Malformed => {}
                openai::ParsedChunk::Done => {
                    stop = true;
                    break;
                }
                openai::ParsedChunk::Error { message, retryable } => {
                    outcome.error = Some(AppError::new(code::SERVER, message).retryable(retryable));
                    stop = true;
                    break;
                }
                openai::ParsedChunk::Parts(parts) => {
                    if let Some(text) = parts.content {
                        outcome.content.push_str(&text);
                        pending_text.push_str(&text);
                    }
                    if let Some(text) = parts.reasoning {
                        outcome.reasoning.push_str(&text);
                        pending_reasoning.push_str(&text);
                    }
                    if let Some(u) = parts.usage {
                        outcome.usage = Some(u);
                    }
                    if let Some(reason) = parts.finish_reason {
                        outcome.finish_reason = Some(reason);
                        finished = true;
                    }
                    for delta in parts.tool_call_deltas {
                        let entry = tool_acc
                            .entry(delta.index)
                            .or_insert_with(|| (None, None, String::new()));
                        if let Some(id) = delta.id {
                            if entry.0.is_none() {
                                entry.0 = Some(id);
                            }
                        }
                        if let Some(name) = delta.name {
                            if entry.1.is_none() {
                                entry.1 = Some(name);
                            }
                        }
                        entry.2.push_str(&delta.arguments);
                    }
                }
            }
        }

        // 关键：**无论上面为什么停下**，都必须把这一段已累积的内容发出去。
        //
        // 这里曾经是 `break 'outer`，结果「`delta` 和 `[DONE]` 落在同一个 TCP
        // 分片里」时，正文进了数据库却从来没推给前端 —— 用户看到一条空消息，
        // 但历史里其实有内容。回归测试见 `content_and_done_in_the_same_chunk`。
        flush(
            &mut sink,
            request_id,
            &mut pending_text,
            &mut pending_reasoning,
        );

        if outcome.error.is_some() {
            break;
        }

        if parser.overflowed() {
            outcome.error = Some(AppError::new(
                code::SERVER,
                "服务端响应异常（单行或单事件过大），已中止",
            ));
            break;
        }
    }

    // 流结束：冲刷最后一个没闭合的事件。
    // 漏掉这一步就会丢掉回答的最后一句 —— 有些服务商不补结尾空行。
    if outcome.error.is_none() && !outcome.cancelled {
        for payload in parser.finish() {
            match openai::parse_stream_chunk(&payload) {
                openai::ParsedChunk::Parts(parts) => {
                    if let Some(text) = parts.content {
                        outcome.content.push_str(&text);
                        sink(ChatEvent::Delta {
                            request_id: request_id.to_string(),
                            text,
                        });
                    }
                    if let Some(text) = parts.reasoning {
                        outcome.reasoning.push_str(&text);
                        sink(ChatEvent::ReasoningDelta {
                            request_id: request_id.to_string(),
                            text,
                        });
                    }
                    if let Some(u) = parts.usage {
                        outcome.usage = Some(u);
                    }
                    if let Some(reason) = parts.finish_reason {
                        outcome.finish_reason = Some(reason);
                    }
                }
                openai::ParsedChunk::Error { message, retryable } => {
                    outcome.error = Some(AppError::new(code::SERVER, message).retryable(retryable));
                }
                _ => {}
            }
        }
    }

    if let Some(u) = &outcome.usage {
        sink(ChatEvent::Usage {
            request_id: request_id.to_string(),
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
        });
    }

    // 组装工具调用：缺 id 或 name 的（畸形服务端）直接丢弃，不 panic。
    outcome.tool_calls = tool_acc
        .into_values()
        .filter_map(|(id, name, arguments)| {
            Some(ToolCall {
                id: id?,
                name: name?,
                arguments,
            })
        })
        .collect();

    outcome
}

fn flush<F: FnMut(ChatEvent)>(
    sink: &mut F,
    request_id: &str,
    text: &mut String,
    reasoning: &mut String,
) {
    if !text.is_empty() {
        sink(ChatEvent::Delta {
            request_id: request_id.to_string(),
            text: std::mem::take(text),
        });
    }
    if !reasoning.is_empty() {
        sink(ChatEvent::ReasoningDelta {
            request_id: request_id.to_string(),
            text: std::mem::take(reasoning),
        });
    }
}

// ------------------------------------------------------------------ 测试
//
// 这些测试**真的开 TCP 端口、真的走 reqwest、真的解析 chunked 编码**。
// 它们覆盖的场景（UTF-8 跨 TCP 读、`[DONE]` 与正文同片、finish 后补 usage、
// 结尾不给空行、401 不重试、500 重试、取消）都是线上真出过问题的形态。

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    struct MockReply {
        status: u16,
        /// 按顺序、每段之间留 `gap_ms` 间隔写出去 —— 模拟 TCP 分片。
        chunks: Vec<Vec<u8>>,
        /// 写完 chunks 后不结束连接（用于测取消）。
        hang: bool,
        /// 相邻两段之间的间隔。
        ///
        /// 做成可配的，是因为「取消」那条测试踩过坑：它原来用默认的 3ms
        /// 间隔 + 「睡 25ms 再取消」，于是**取消和第一片到达在赛跑** ——
        /// 机器一忙（并行跑测试时）第一片还没处理就取消了，测试随机失败。
        /// 现在那条测试把间隔拉到 500ms、睡到 150ms 再取消，
        /// 两边都留出足够余量，不再依赖调度运气。
        gap_ms: u64,
    }

    impl MockReply {
        fn ok(chunks: Vec<Vec<u8>>) -> Self {
            Self {
                status: 200,
                chunks,
                hang: false,
                gap_ms: 3,
            }
        }
        fn status(status: u16, body: &str) -> Self {
            Self {
                status,
                chunks: vec![body.as_bytes().to_vec()],
                hang: false,
                gap_ms: 3,
            }
        }
        /// 挂住不结束（用于测取消），两片之间隔 `gap_ms`。
        ///
        /// 间隔做成参数而不是写死 3ms：见 [`Self::gap_ms`] 的注释 ——
        /// 「睡一小会儿再取消」这种写法必须和它拉开足够距离，否则就是赛跑。
        fn hanging(chunks: Vec<Vec<u8>>, gap_ms: u64) -> Self {
            Self {
                status: 200,
                chunks,
                hang: true,
                gap_ms,
            }
        }
    }

    fn sse(payload: &str) -> Vec<u8> {
        format!("data: {payload}\n\n").into_bytes()
    }

    /// 与 `sse` 相对：**不带**结尾空行，模拟服务端发完就走。
    fn sse_unterminated(payload: &str) -> Vec<u8> {
        format!("data: {payload}").into_bytes()
    }

    fn delta(content: &str) -> Vec<u8> {
        sse(&format!(
            r#"{{"choices":[{{"delta":{{"content":"{content}"}},"index":0}}]}}"#
        ))
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    /// 把请求读干净再回包。
    ///
    /// 这不是洁癖：Windows 上如果关闭 socket 时接收缓冲区里还有没读完的数据，
    /// 内核会发 RST，而 RST 可能把我们刚写出去的响应一起干掉 —— 测试就会
    /// 随机失败。
    async fn drain_request(socket: &mut tokio::net::TcpStream) {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 2048];
        let header_end = loop {
            match socket.read(&mut tmp).await {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(pos) = find(&buf, b"\r\n\r\n") {
                        break pos + 4;
                    }
                    if buf.len() > 128 * 1024 {
                        return;
                    }
                }
            }
        };

        let head = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
        let want = head
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0);

        let mut have = buf.len() - header_end;
        while have < want {
            match socket.read(&mut tmp).await {
                Ok(0) | Err(_) => break,
                Ok(n) => have += n,
            }
        }
    }

    /// 起一个本地假服务端：第 N 个连接返回 `replies[N]`（越界则重复最后一个）。
    /// 返回 `(url, 收到的请求数)`。
    async fn mock_server(replies: Vec<MockReply>) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let index = counter.fetch_add(1, Ordering::SeqCst);
                let last = replies.len().saturating_sub(1);
                let Some(reply) = replies.get(index.min(last)) else {
                    break;
                };

                drain_request(&mut socket).await;

                let header = format!(
                    "HTTP/1.1 {} X\r\n\
                     Content-Type: text/event-stream\r\n\
                     Cache-Control: no-cache\r\n\
                     Connection: close\r\n\
                     Transfer-Encoding: chunked\r\n\r\n",
                    reply.status
                );
                if socket.write_all(header.as_bytes()).await.is_err() {
                    continue;
                }

                for chunk in &reply.chunks {
                    let framed = format!("{:x}\r\n", chunk.len());
                    if socket.write_all(framed.as_bytes()).await.is_err()
                        || socket.write_all(chunk).await.is_err()
                        || socket.write_all(b"\r\n").await.is_err()
                    {
                        break;
                    }
                    let _ = socket.flush().await;
                    tokio::time::sleep(Duration::from_millis(reply.gap_ms)).await;
                }

                if reply.hang {
                    // 保持连接但不再发数据，让客户端停在「等下一片」上。
                    tokio::time::sleep(Duration::from_secs(30)).await;
                } else {
                    let _ = socket.write_all(b"0\r\n\r\n").await;
                    let _ = socket.flush().await;
                    let _ = socket.shutdown().await;
                }
            }
        });

        (format!("http://{addr}/v1/chat/completions"), hits)
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .expect("client")
    }

    fn body() -> Value {
        openai::build_stream_body("test-model", &[], None, None, None, None)
    }

    /// 跑完整链路：发送（含重试）→ 消费 → 收集事件。
    async fn run(url: &str, token: &CancellationToken) -> (StreamOutcome, Vec<ChatEvent>) {
        let response = send_with_retry(&client(), url, "test-key", &body(), token)
            .await
            .expect("send should succeed");
        let mut events = Vec::new();
        let outcome = consume_stream(response, "req-test", token, |event| events.push(event)).await;
        (outcome, events)
    }

    fn deltas(events: &[ChatEvent]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                ChatEvent::Delta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn basic_stream_reassembles_chinese_content() {
        let (url, hits) = mock_server(vec![MockReply::ok(vec![
            delta("你好"),
            delta("，世界"),
            sse("[DONE]"),
        ])])
        .await;

        let token = CancellationToken::new();
        let (outcome, events) = run(&url, &token).await;

        assert_eq!(outcome.content, "你好，世界");
        assert_eq!(deltas(&events), "你好，世界");
        assert!(outcome.error.is_none());
        assert!(!outcome.cancelled);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn utf8_split_across_tcp_reads_survives() {
        // 把一整个 SSE 事件在「你」的第二个字节处切开，分两个 TCP 段发出。
        let full = delta("你好世界");
        let cut = full
            .windows(3)
            .position(|w| w == "你".as_bytes())
            .expect("find 你")
            + 1; // 切在多字节字符中间

        let (url, _) = mock_server(vec![MockReply::ok(vec![
            full[..cut].to_vec(),
            full[cut..].to_vec(),
            sse("[DONE]"),
        ])])
        .await;

        let token = CancellationToken::new();
        let (outcome, events) = run(&url, &token).await;

        assert_eq!(outcome.content, "你好世界", "半截 UTF-8 不能变成替换字符");
        assert!(!deltas(&events).contains('\u{FFFD}'), "不允许出现 �");
    }

    #[tokio::test]
    async fn usage_arriving_after_finish_reason_is_captured() {
        let (url, _) = mock_server(vec![MockReply::ok(vec![
            sse(r#"{"choices":[{"delta":{"content":"答案"},"finish_reason":"stop"}]}"#),
            // OpenAI 会在带 finish_reason 的**下一片**才发 usage。
            sse(r#"{"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":9}}"#),
        ])])
        .await;

        let token = CancellationToken::new();
        let (outcome, events) = run(&url, &token).await;

        assert_eq!(outcome.finish_reason.as_deref(), Some("stop"));
        let usage = outcome.usage.expect("usage 必须在宽限期内被捕获");
        assert_eq!(
            (usage.input_tokens, usage.output_tokens),
            (Some(7), Some(9))
        );
        assert!(
            events.iter().any(|e| matches!(e, ChatEvent::Usage { .. })),
            "usage 事件也要推给前端"
        );
    }

    #[tokio::test]
    async fn empty_finish_reason_does_not_end_the_stream_early() {
        // 有的服务端每一帧都带 `"finish_reason": ""`。
        // 若把它当成结束，用户只会看到一个字的回答。
        let (url, _) = mock_server(vec![MockReply::ok(vec![
            sse(r#"{"choices":[{"delta":{"content":"一"},"finish_reason":""}]}"#),
            sse(r#"{"choices":[{"delta":{"content":"二"},"finish_reason":""}]}"#),
            sse(r#"{"choices":[{"delta":{"content":"三"},"finish_reason":""}]}"#),
            sse("[DONE]"),
        ])])
        .await;

        let token = CancellationToken::new();
        let (outcome, _) = run(&url, &token).await;

        assert_eq!(outcome.content, "一二三");
        assert_eq!(outcome.finish_reason, None, "空串不算结束原因");
    }

    #[tokio::test]
    async fn content_and_done_in_the_same_chunk_is_not_lost() {
        // 回归测试：这里曾经是 `break 'outer`，会跳过 flush，
        // 于是正文进了库却从来没推给前端 —— 界面一条空消息。
        let mut one_chunk = delta("最后一句");
        one_chunk.extend_from_slice(&sse("[DONE]"));

        let (url, _) = mock_server(vec![MockReply::ok(vec![one_chunk])]).await;

        let token = CancellationToken::new();
        let (outcome, events) = run(&url, &token).await;

        assert_eq!(outcome.content, "最后一句");
        assert_eq!(
            deltas(&events),
            "最后一句",
            "[DONE] 与正文同片时正文不能被吞掉"
        );
    }

    #[tokio::test]
    async fn unterminated_last_event_is_flushed_on_close() {
        // 服务端发完最后一段就直接关连接，不补那个空行。
        // 少了 `parser.finish()`，用户就会丢掉回答的最后一句。
        let (url, _) = mock_server(vec![MockReply::ok(vec![
            delta("开头"),
            // 注意：故意**不**带结尾的空行。
            sse_unterminated(r#"{"choices":[{"delta":{"content":"没有空行"}}]}"#),
        ])])
        .await;

        let token = CancellationToken::new();
        let (outcome, events) = run(&url, &token).await;

        assert_eq!(outcome.content, "开头没有空行");
        assert_eq!(deltas(&events), "开头没有空行");
    }

    #[tokio::test]
    async fn reasoning_deltas_are_separated_from_content() {
        let (url, _) = mock_server(vec![MockReply::ok(vec![
            sse(r#"{"choices":[{"delta":{"reasoning_content":"让我想想"}}]}"#),
            sse(r#"{"choices":[{"delta":{"content":"答案是 42"}}]}"#),
            sse("[DONE]"),
        ])])
        .await;

        let token = CancellationToken::new();
        let (outcome, events) = run(&url, &token).await;

        assert_eq!(outcome.content, "答案是 42");
        assert_eq!(outcome.reasoning, "让我想想");
        assert!(events
            .iter()
            .any(|e| matches!(e, ChatEvent::ReasoningDelta { .. })));
    }

    #[tokio::test]
    async fn in_stream_error_is_reported_and_partial_content_kept() {
        let (url, _) = mock_server(vec![MockReply::ok(vec![
            delta("已经写了一半"),
            sse(r#"{"error":{"message":"余额不足"}}"#),
        ])])
        .await;

        let token = CancellationToken::new();
        let (outcome, events) = run(&url, &token).await;

        let error = outcome.error.expect("应当报错");
        assert!(error.message.contains("余额不足"), "{}", error.message);
        assert_eq!(outcome.content, "已经写了一半", "半截回答要保留");
        assert_eq!(deltas(&events), "已经写了一半", "半截回答也要推给前端");
    }

    #[tokio::test]
    async fn tool_calls_are_accumulated_across_chunks() {
        // 原生 function calling：参数是分片到达的，id / name 只在第一片出现。
        let (url, _) = mock_server(vec![MockReply::ok(vec![
            sse(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"calculator","arguments":"{\"expr"}}]}}]}"#),
            sse(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ession\":\"1+1\"}"}}]},"finish_reason":"tool_calls"}]}"#),
            sse("[DONE]"),
        ])])
        .await;

        let token = CancellationToken::new();
        let (outcome, _) = run(&url, &token).await;

        assert_eq!(outcome.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(outcome.tool_calls.len(), 1);
        assert_eq!(outcome.tool_calls[0].id, "call_1");
        assert_eq!(outcome.tool_calls[0].name, "calculator");
        // 两片参数按 index 拼回完整的 JSON。
        assert_eq!(outcome.tool_calls[0].arguments, r#"{"expression":"1+1"}"#);
    }

    #[tokio::test]
    async fn server_errors_are_retried_before_the_stream_starts() {
        let (url, hits) = mock_server(vec![
            MockReply::status(500, r#"{"error":{"message":"boom"}}"#),
            MockReply::status(500, r#"{"error":{"message":"boom"}}"#),
            MockReply::ok(vec![delta("终于成功"), sse("[DONE]")]),
        ])
        .await;

        let token = CancellationToken::new();
        let (outcome, _) = run(&url, &token).await;

        assert_eq!(outcome.content, "终于成功");
        assert_eq!(hits.load(Ordering::SeqCst), 3, "5xx 应当重试，共 3 次");
    }

    #[tokio::test]
    async fn unauthorized_is_not_retried() {
        let (url, hits) = mock_server(vec![MockReply::status(
            401,
            r#"{"error":{"message":"Invalid API key"}}"#,
        )])
        .await;

        let token = CancellationToken::new();
        let client = client();
        let err = send_with_retry(&client, &url, "bad-key", &body(), &token)
            .await
            .expect_err("401 必须失败");

        assert_eq!(err.code, code::UNAUTHORIZED);
        assert!(!err.retryable);
        assert_eq!(hits.load(Ordering::SeqCst), 1, "401 不该重试");
    }

    #[tokio::test]
    async fn a_broken_stream_is_not_retried() {
        // 声明 200 字节的 chunk，只发 10 字节就关连接 → 客户端读到一半会报错。
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();

        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                drain_request(&mut socket).await;
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 200 X\r\nContent-Type: text/event-stream\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n",
                    )
                    .await;
                // 声称 200 字节，实际只给 10 字节，然后走人。
                let _ = socket.write_all(b"c8\r\n").await;
                let _ = socket.write_all(b"data: {\"c").await;
                let _ = socket.flush().await;
                let _ = socket.shutdown().await;
            }
        });

        let url = format!("http://{addr}/v1/chat/completions");
        let token = CancellationToken::new();
        let response = send_with_retry(&client(), &url, "k", &body(), &token)
            .await
            .expect("2xx 已经返回，send 不该失败");

        let outcome = consume_stream(response, "r", &token, |_| {}).await;
        assert!(outcome.error.is_some(), "截断的 chunked 流必须报错");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "流已经开始，绝不能重试（否则回答会重复拼接）"
        );
    }

    #[tokio::test]
    async fn cancelling_stops_the_stream_and_keeps_partial_content() {
        // 两片之间隔 500ms，第一片几乎是立刻到的 —— 这样「睡 150ms 再取消」
        // 在两边都有很大余量：第一片一定已经处理完，第二片一定还没发。
        //
        // 原来这里用的是默认 3ms 间隔 + 睡 25ms：取消和第一片到达在赛跑，
        // 并行跑测试、机器一忙就会随机失败（而且失败信息看起来像产品 bug）。
        let (url, _) = mock_server(vec![MockReply::hanging(
            vec![delta("先给你一半"), delta("这一半不该到达")],
            500,
        )])
        .await;

        let token = CancellationToken::new();
        let cancel_from = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            cancel_from.cancel();
        });

        let (outcome, events) = run(&url, &token).await;

        assert!(outcome.cancelled, "应当以“已取消”结束");
        // 现在可以断言**正好**只收到第一片 —— 这才是这条测试想表达的东西。
        assert_eq!(
            outcome.content, "先给你一半",
            "已到达的内容要保留，后面的不该被写进去"
        );
        assert!(outcome.error.is_none());
        assert_eq!(deltas(&events), outcome.content);
    }

    #[tokio::test]
    async fn cancelling_during_backoff_returns_immediately() {
        // 服务端一直 500（可重试），用户在退避期间点停止 —— 不该干等 1.5 秒。
        let (url, _) = mock_server(vec![MockReply::status(500, "{}")]).await;

        let token = CancellationToken::new();
        let cancel_from = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_from.cancel();
        });

        let started = std::time::Instant::now();
        let err = send_with_retry(&client(), &url, "k", &body(), &token)
            .await
            .expect_err("应当以取消结束");
        let elapsed = started.elapsed();

        assert_eq!(err.code, code::CANCELLED);
        assert!(
            elapsed < Duration::from_millis(500),
            "取消后应当立刻返回，实际耗时 {elapsed:?}"
        );
    }
}
