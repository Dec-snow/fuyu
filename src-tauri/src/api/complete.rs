//! 一次性（非流式）补全：给滚动摘要这类「后台小任务」用。
//!
//! 为什么不复用流式那条路径：摘要要的是**完整结果**，不是逐字上屏。走流式
//! 只会白白多出 SSE 解析与事件广播的开销，而且中途断开还会拿到半截 JSON。

use std::time::Duration;

use serde_json::Value;

use crate::api::{openai, MAX_ERROR_BODY};
use crate::errors::{code, read_body_capped, AppError, AppResult};

/// 发一次非流式请求，返回助手回复的正文。
///
/// `api_key` 为空时不发 `Authorization`（本地 Ollama 之类不需要）。
pub async fn complete_once(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    body: &Value,
    timeout: Duration,
) -> AppResult<String> {
    let mut http = client
        .post(url)
        .header("Content-Type", "application/json")
        .json(body);
    if !api_key.is_empty() {
        http = http.bearer_auth(api_key);
    }

    let deadline_message = format!("请求超时（{} 秒）", timeout.as_secs());

    // 超时必须包住**整次交互**（建连 + 发请求 + 读响应体），不能只包 `send()`：
    // 一个只回响应头、然后挂着不吐 body 的服务端（企业代理、坏掉的中转都很常见）
    // 会让 `send()` 立刻成功，而读 body 永远不返回 ——「测试连接」就一直转下去，
    // 用户除了杀进程没有别的办法。
    tokio::time::timeout(timeout, async move {
        let response = http.send().await?;

        let status = response.status();
        let payload = read_body_capped(response, MAX_ERROR_BODY).await?;
        if !status.is_success() {
            return Err(openai::map_http_error(status.as_u16(), &payload));
        }
        openai::extract_completion_text(&payload)
    })
    .await
    .map_err(|_| AppError::new(code::TIMEOUT, deadline_message).retryable(true))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// 起一个只应答一次的小服务端，返回 (url, 拿到的原始请求文本)。
    ///
    /// 刻意不用流式测试里那套 chunked 服务端：非流式响应带 `Content-Length`，
    /// 这里要验证的正是「不需要 SSE 解析也能拿到完整正文」。
    async fn mock_once(status: u16, body: &str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = body.to_string();

        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            // 请求很小且一次就能读完，读一次足够断言头部。
            let read = socket.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..read]).to_string();

            let response = format!(
                "HTTP/1.1 {status} X\r\n\
                 Content-Type: application/json\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.flush().await;
            let _ = socket.shutdown().await;
            request
        });

        (format!("http://{addr}/v1/chat/completions"), handle)
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .expect("client")
    }

    fn body() -> Value {
        json!({ "model": "m", "messages": [], "stream": false })
    }

    #[tokio::test]
    async fn returns_the_assistant_text_verbatim() {
        let (url, handle) = mock_once(
            200,
            r#"{"choices":[{"message":{"role":"assistant","content":"摘要正文"}}]}"#,
        )
        .await;

        let text = complete_once(&client(), &url, "sk-test", &body(), Duration::from_secs(5))
            .await
            .expect("应当成功");

        assert_eq!(text, "摘要正文");

        // 请求本身要带 key，并且明确是非流式。
        let request = handle.await.unwrap().to_lowercase();
        assert!(
            request.contains("authorization: bearer sk-test"),
            "缺少鉴权头：{request}"
        );
        assert!(request.contains("\"stream\":false"), "应当是流式之外的形态");
    }

    #[tokio::test]
    async fn an_empty_api_key_sends_no_authorization_header() {
        // 本地 Ollama 之类不需要 key；不能凭空塞一个空的 Authorization。
        let (url, handle) = mock_once(200, r#"{"choices":[{"message":{"content":"ok"}}]}"#).await;

        complete_once(&client(), &url, "", &body(), Duration::from_secs(5))
            .await
            .expect("应当成功");

        let request = handle.await.unwrap().to_lowercase();
        assert!(
            !request.contains("authorization"),
            "空 key 不该发鉴权头：{request}"
        );
    }

    /// 只回响应头就挂住的服务端：`send()` 会成功，但 body 永远不来。
    /// 这正是「超时只包 send()」时会让「测试连接」一直转下去的场景。
    async fn mock_stalling_body() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let _ = socket.read(&mut buf).await;
            // 声明 100 字节，然后一个字节都不发。
            // 注意别用 `\` 续行：字节串里的续行会把下一行的缩进一起发出去。
            let header =
                "HTTP/1.1 200 X\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n";
            let _ = socket.write_all(header.as_bytes()).await;
            let _ = socket.flush().await;
            // 挂着不关连接。
            tokio::time::sleep(Duration::from_secs(30)).await;
        });

        format!("http://{addr}/v1/chat/completions")
    }

    #[tokio::test]
    async fn a_stalling_body_times_out_instead_of_hanging_forever() {
        let url = mock_stalling_body().await;
        let started = std::time::Instant::now();

        let error = complete_once(&client(), &url, "k", &body(), Duration::from_millis(300))
            .await
            .expect_err("读 body 挂住时必须超时");

        assert_eq!(error.code, code::TIMEOUT);
        assert!(error.retryable, "超时是可重试的");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "超时必须真的生效，实际等了 {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn an_error_status_becomes_a_readable_app_error() {
        let (url, _handle) = mock_once(401, r#"{"error":{"message":"无效的 API Key"}}"#).await;

        let error = complete_once(&client(), &url, "bad", &body(), Duration::from_secs(5))
            .await
            .expect_err("非 2xx 必须报错");

        assert_eq!(error.code, code::UNAUTHORIZED);
        assert!(
            error.message.contains("API Key") || error.message.contains("401"),
            "错误信息要能看懂：{}",
            error.message
        );
    }
}
