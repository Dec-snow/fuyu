//! 文生图命令。
//!
//! 复用用户已经配好的服务商（Base URL + API Key），只把端点从
//! `chat/completions` 换成 `images/generations` —— 不需要引入「图片服务商」
//! 这个新概念。

use serde::Serialize;
use tauri::State;

use crate::api::image;
use crate::api::openai;
use crate::errors::{code, AppError, AppResult};
use crate::state::AppState;
use crate::storage::config::ConfigStore;
use crate::storage::database::Database;
use crate::storage::secrets;

/// 提示词长度上限。
///
/// OpenAI 的图片接口就是 4000 字符；与其让服务商回一个 400，不如在本地先拦下来
/// 并说清楚 —— 用户面对「请求被拒绝」根本不知道是自己写太长。
const MAX_PROMPT_CHARS: usize = 4000;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedImage {
    /// `data:image/png;base64,...`，直接就能放进消息的图片字段。
    pub data_url: String,
    pub mime: String,
    pub bytes: usize,
    pub model: String,
    /// 用户原本写的提示词。
    pub prompt: String,
    /// 服务商回写的、它实际用的提示词（有的会改写）。
    pub revised_prompt: Option<String>,
}

#[tauri::command]
pub async fn generate_image(
    state: State<'_, AppState>,
    config: State<'_, ConfigStore>,
    prompt: String,
    size: Option<String>,
) -> AppResult<GeneratedImage> {
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err(AppError::invalid("先描述一下想画什么"));
    }
    if prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err(AppError::invalid(format!(
            "描述太长了（{} 字，上限 {MAX_PROMPT_CHARS} 字）",
            prompt.chars().count()
        )));
    }

    let (provider, model, configured_size) = config.read(|cfg| {
        let provider = cfg
            .provider(&cfg.image_provider_id)
            .cloned()
            // 没配就用默认服务商，省得用户在两个地方各选一遍
            .or_else(|| {
                cfg.default_provider_id
                    .as_deref()
                    .and_then(|id| cfg.provider(id))
                    .cloned()
            });
        (
            provider,
            cfg.image_model.trim().to_string(),
            cfg.image_size.clone(),
        )
    });

    let provider = provider
        .ok_or_else(|| AppError::invalid("还没有可用的服务商，请先到「设置 → 模型」里配置"))?;
    if model.is_empty() {
        return Err(AppError::invalid(
            "还没有指定画图的模型，请到「设置 → 模型 → 文生图」里填写",
        ));
    }

    let api_key = secrets::get_api_key(&provider.id)
        .map_err(AppError::secret)?
        .unwrap_or_default();
    if api_key.is_empty() && !provider.is_local_endpoint() {
        return Err(AppError::new(
            code::UNAUTHORIZED,
            format!(
                "{} 还没有配置 API Key，请到「设置 → 模型」里填写",
                provider.name
            ),
        ));
    }

    let url = image::images_url(&provider.base_url)?;
    // 请求里传了就用它，否则用设置里那个
    let size = image::validate_size(
        size.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(configured_size.as_str()),
    )?;
    let client = state.client();

    let (bytes, mime, revised_prompt) =
        fetch_image(&client, &url, &api_key, &model, &prompt, &size).await?;

    Ok(GeneratedImage {
        data_url: image::to_data_url(mime, &bytes),
        mime: mime.to_string(),
        bytes: bytes.len(),
        model,
        prompt,
        revised_prompt,
    })
}

/// 把生成的图片写进会话：一条用户消息（就是那句描述）+ 一条带图的助手消息。
///
/// 为什么由后端一次做完，而不是前端分两步调 `saveUserMessage` 再存图：
/// 分两步就会出现「用户消息写进去了、图片那步失败了」的中间状态 ——
/// 会话里留下一条没有回答的提问。一个事务里做完，要么都进要么都不进。
#[tauri::command]
pub async fn append_image_turn(
    db: State<'_, Database>,
    conversation_id: String,
    prompt: String,
    image: String,
    model: String,
) -> AppResult<AdoptedImage> {
    let prompt = prompt.trim().to_string();
    if image.trim().is_empty() {
        return Err(AppError::invalid("图片内容为空"));
    }
    if db.get_conversation(&conversation_id)?.is_none() {
        return Err(AppError::invalid("会话不存在了（可能已被删除）"));
    }

    let user_message_id = if prompt.is_empty() {
        String::new()
    } else {
        db.insert_message(
            &conversation_id,
            "user",
            &prompt,
            &[],
            None,
            None,
            None,
            None,
            None,
        )?
    };
    // 助手那条的正文是一句说明，图片挂在 `images` 上 —— 这样它在历史里
    // 既能被搜索到（有文字），又能被渲染成图（有图）。
    let caption = format!("🎨 已按「{prompt}」生成图片");
    let assistant_message_id = db.insert_message(
        &conversation_id,
        "assistant",
        &caption,
        std::slice::from_ref(&image),
        None,
        None,
        Some(&model),
        None,
        None,
    )?;
    db.touch_conversation_only(&conversation_id)?;

    Ok(AdoptedImage {
        user_message_id,
        assistant_message_id,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdoptedImage {
    pub user_message_id: String,
    pub assistant_message_id: String,
}

/// 发请求 → 必要时去掉 `response_format` 重试 → 拿到图片字节。
///
/// 抽出来是为了能测：命令本身要 `State<AppState>`，在测试里构造不出来，
/// 而这段「重试 + 解析 + 下载」恰恰是最值得用真 TCP 服务器验一遍的部分。
async fn fetch_image(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    size: &str,
) -> AppResult<(Vec<u8>, &'static str, Option<String>)> {
    // 先带上 response_format；被明确拒绝就去掉再试一次。
    // 这不是可选优化：gpt-image-1 收到这个字段会直接 400，而 DALL·E 一类的模型
    // 靠它决定返回 base64 还是 URL —— 两边都要能用。
    let body = image::build_image_body(model, prompt, size, true);
    let mut response = post_json(client, url, api_key, &body).await?;

    let status = response.status().as_u16();
    if status == 400 || status == 422 {
        let text = response.text().await.unwrap_or_default();
        if image::looks_like_response_format_rejection(status, &text) {
            let retry = image::build_image_body(model, prompt, size, false);
            response = post_json(client, url, api_key, &retry).await?;
        } else {
            return Err(openai::map_http_error(status, &text));
        }
    }

    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(openai::map_http_error(status, &text));
    }

    let parsed = image::parse_image_response(&text)?;
    match parsed.payload {
        image::ImagePayload::Base64(data) => {
            use base64::engine::general_purpose::STANDARD;
            use base64::Engine as _;
            let bytes = STANDARD
                .decode(data.trim())
                .map_err(|e| AppError::new(code::SERVER, format!("图片 base64 解不开：{e}")))?;
            let mime = image::validate_image_bytes(&bytes)?;
            Ok((bytes, mime, parsed.revised_prompt))
        }
        image::ImagePayload::Url(image_url) => {
            let (bytes, mime) = download_image(client, &image_url).await?;
            Ok((bytes, mime, parsed.revised_prompt))
        }
    }
}

async fn post_json(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    body: &serde_json::Value,
) -> AppResult<reqwest::Response> {
    client
        .post(url)
        .bearer_auth(api_key)
        .json(body)
        .send()
        .await
        .map_err(|e| AppError::network(format!("连不上服务商：{e}")))
}

/// 下载服务商给的图片 URL。
///
/// 两个刻意的约束：
///
/// - **不带 API Key**。这个 URL 常常指向第三方 CDN，把密钥发过去等于把凭据
///   交给了不相关的域。图片下载本来就不需要鉴权（需要鉴权的服务商会直接返回
///   base64）。
/// - **先看 Content-Length 再读**，并且读完再核对一次实际长度。`resp.bytes()`
///   会把整个响应读进内存，一个被构造出来的超长响应就是一次内存打满的机会。
async fn download_image(client: &reqwest::Client, url: &str) -> AppResult<(Vec<u8>, &'static str)> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(AppError::new(
            code::SERVER,
            format!("服务商给的图片地址不是 http(s)：{url}"),
        ));
    }

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| AppError::network(format!("下载生成的图片失败：{e}")))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(AppError::new(
            code::SERVER,
            format!("下载生成的图片失败（HTTP {status}）"),
        ));
    }
    if let Some(len) = response.content_length() {
        if len as usize > image::MAX_IMAGE_BYTES {
            return Err(AppError::new(
                code::SERVER,
                format!(
                    "生成的图片太大了（{:.1} MB，上限 {} MB）",
                    len as f64 / 1024.0 / 1024.0,
                    image::MAX_IMAGE_BYTES / 1024 / 1024
                ),
            ));
        }
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| AppError::network(format!("读取图片内容失败：{e}")))?
        .to_vec();
    let mime = image::validate_image_bytes(&bytes)?;
    Ok((bytes, mime))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// 一张最小的合法 PNG（8 字节魔数 + 一点内容）。
    fn tiny_png() -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.extend_from_slice(b"fake-but-sniffable");
        bytes
    }

    /// 起一个只处理图片接口的假服务商。
    ///
    /// `handler` 收到 (第几次请求, 请求体) 返回 (状态码, 响应体)。
    #[allow(clippy::type_complexity)]
    fn spawn_server(
        handler: Arc<dyn Fn(usize, &str) -> (u16, String) + Send + Sync>,
    ) -> (String, Arc<AtomicUsize>, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();

        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buffer = [0u8; 8192];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                // 请求体在空行之后
                let body = request
                    .split_once("\r\n\r\n")
                    .map(|(_, b)| b.to_string())
                    .unwrap_or_default();

                let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                let (status, payload) = handler(n, &body);

                let response = format!(
                    "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    if status == 200 { "OK" } else { "Bad Request" },
                    payload.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });

        (
            format!("http://127.0.0.1:{port}/v1/images/generations"),
            count,
            handle,
        )
    }

    #[tokio::test]
    async fn a_response_format_rejection_is_retried_without_the_field() {
        // 真开一个 TCP 端口，验证「gpt-image-1 那类模型会拒 response_format」的
        // 整条重试路径：第一次 400、去掉字段后第二次成功。
        let png_b64 = {
            use base64::engine::general_purpose::STANDARD;
            use base64::Engine as _;
            STANDARD.encode(tiny_png())
        };
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let recorder = seen.clone();
        let b64 = png_b64.clone();

        let (url, count, _handle) = spawn_server(Arc::new(move |n, body| {
            recorder.lock().unwrap().push(body.to_string());
            if n == 1 {
                (
                    400,
                    r#"{"error":{"message":"Unknown parameter: 'response_format'."}}"#.to_string(),
                )
            } else {
                (200, format!(r#"{{"data":[{{"b64_json":"{b64}"}}]}}"#))
            }
        }));

        let client = reqwest::Client::new();
        let result = fetch_image(
            &client,
            &url,
            "sk-test",
            "gpt-image-1",
            "一只猫",
            "1024x1024",
        )
        .await
        .expect("第二次应当成功");

        assert_eq!(result.0, tiny_png());
        assert_eq!(result.1, "image/png");
        assert_eq!(count.load(Ordering::SeqCst), 2, "应当恰好重试一次");

        let bodies = seen.lock().unwrap();
        assert!(
            bodies[0].contains("response_format"),
            "第一次要带上：{}",
            bodies[0]
        );
        assert!(
            !bodies[1].contains("response_format"),
            "重试要去掉这个字段：{}",
            bodies[1]
        );
        // 重试不能把别的字段弄丢
        assert!(bodies[1].contains("一只猫"));
        assert!(bodies[1].contains("1024x1024"));
    }

    #[tokio::test]
    async fn an_unrelated_400_is_not_retried() {
        // 别的 400 不能触发重试 —— 那只是白烧一次请求
        let (url, count, _handle) = spawn_server(Arc::new(|_n, _body| {
            (
                400,
                r#"{"error":{"message":"content policy violation"}}"#.to_string(),
            )
        }));
        let client = reqwest::Client::new();
        let err = fetch_image(
            &client,
            &url,
            "sk-test",
            "dall-e-3",
            "画点什么",
            "1024x1024",
        )
        .await
        .unwrap_err();
        assert!(err.message.contains("content policy"), "{}", err.message);
        assert_eq!(count.load(Ordering::SeqCst), 1, "不该重试");
    }

    #[tokio::test]
    async fn an_empty_image_is_rejected_instead_of_being_used() {
        // 服务商返回了「成功」但没有图片数据：必须报错，不能让一条空图进聊天
        let (url, _count, _handle) =
            spawn_server(Arc::new(|_n, _b| (200, r#"{"data":[]}"#.to_string())));
        let client = reqwest::Client::new();
        let err = fetch_image(
            &client,
            &url,
            "sk-test",
            "dall-e-3",
            "画点什么",
            "1024x1024",
        )
        .await
        .unwrap_err();
        assert!(!err.message.is_empty());
    }
}
