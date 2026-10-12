//! 文生图适配器（OpenAI `images/generations` 兼容）。
//!
//! 与 `openai.rs` 同一个思路：**只做构造请求体与归一化响应**，
//! 不碰字节流、不碰 HTTP 客户端 —— 那些在命令层。
//!
//! 为什么挑这个协议：`POST /images/generations` 已经是事实标准，OpenAI、智谱、
//! 通义、SiliconFlow 等一大批服务商都实现了它。于是「文生图」不需要引入新的
//! 服务商概念 —— 复用用户已经配好的那份 Base URL 与 API Key 就够了。

use serde_json::{json, Value};

use crate::errors::{code, AppError, AppResult};
use crate::storage::config::normalize_base_url;

/// 没有指定尺寸时用它。
pub const DEFAULT_SIZE: &str = "1024x1024";

/// 单张图片的字节上限。
///
/// 生成式图片动辄几 MB，而它要以 base64 的形式塞进消息里（再翻 1.33 倍）、
/// 存进数据库、每次读会话都要读出来 —— 不设上限的话，几张图就能让整个会话
/// 变得又慢又占地方。12 MB 足够覆盖 1024/1792 这一档的 PNG。
pub const MAX_IMAGE_BYTES: usize = 12 * 1024 * 1024;

/// 尺寸参数的合法形态：`宽x高`，两个十进制数。
///
/// 不做「白名单」而是做形态校验：各家支持的档位并不一致（有的只给 1024x1024，
/// 有的支持 1024x1792），写死白名单等于替服务商做决定、还容易过时。
/// 但形态必须校验 —— 把 `1024*1024` 或 `大图` 发出去只会换来一个语焉不详的 400。
pub fn validate_size(size: &str) -> AppResult<String> {
    let size = size.trim();
    if size.is_empty() {
        return Ok(DEFAULT_SIZE.to_string());
    }
    let mut parts = size.split('x');
    let (Some(w), Some(h), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(AppError::invalid(format!(
            "图片尺寸要写成「宽x高」，例如 1024x1024，收到的是「{size}」"
        )));
    };
    let ok = |s: &str| !s.is_empty() && s.len() <= 5 && s.chars().all(|c| c.is_ascii_digit());
    if !ok(w) || !ok(h) {
        return Err(AppError::invalid(format!(
            "图片尺寸要写成「宽x高」，例如 1024x1024，收到的是「{size}」"
        )));
    }
    // 挡掉离谱的值：0 会被服务商拒，几万像素纯粹是浪费一次请求
    let (w, h) = (w.parse::<u32>().unwrap_or(0), h.parse::<u32>().unwrap_or(0));
    if w == 0 || h == 0 {
        return Err(AppError::invalid("图片尺寸不能是 0"));
    }
    if w > 4096 || h > 4096 {
        return Err(AppError::invalid(format!(
            "图片尺寸上限是 4096x4096，收到的是「{size}」"
        )));
    }
    Ok(format!("{w}x{h}"))
}

/// 拼出 `images/generations` 端点。
pub fn images_url(base_url: &str) -> AppResult<String> {
    let base = normalize_base_url(base_url)?;
    Ok(format!("{base}/images/generations"))
}

/// 构造请求体。
///
/// `include_response_format` 为假时不带 `response_format` —— 这不是可选优化，
/// 而是**必要的兼容**：OpenAI 的 gpt-image-1 收到这个字段会直接 400
/// （它总是返回 base64，不再接受这个参数），而 DALL·E 一类的模型则靠它
/// 决定返回 base64 还是 URL。所以先带上，被拒了就**去掉再试一次**。
pub fn build_image_body(
    model: &str,
    prompt: &str,
    size: &str,
    include_response_format: bool,
) -> Value {
    let mut body = json!({
        "model": model,
        "prompt": prompt,
        "n": 1,
        "size": size,
    });
    if include_response_format {
        body["response_format"] = json!("b64_json");
    }
    body
}

/// 服务商返回的图片载荷。
#[derive(Debug, Clone, PartialEq)]
pub enum ImagePayload {
    /// 直接给了 base64（最常见）。
    Base64(String),
    /// 给了一个临时 URL，需要再去下载。
    Url(String),
}

/// 归一化后的响应。
#[derive(Debug, Clone, PartialEq)]
pub struct ImageResponse {
    pub payload: ImagePayload,
    /// 有的服务商会回写「它实际用的提示词」。
    pub revised_prompt: Option<String>,
}

/// 解析响应体。
///
/// 两种常见形态：
/// - `{ "data": [ { "b64_json": "..." } ] }`
/// - `{ "data": [ { "url": "https://..." } ] }`
///
/// 还有的服务商把图片放在 `data[0].image`（例如某些兼容层），一并认掉。
pub fn parse_image_response(body: &str) -> AppResult<ImageResponse> {
    let value: Value = serde_json::from_str(body)
        .map_err(|e| AppError::new(code::SERVER, format!("服务商返回的不是合法 JSON：{e}")))?;

    let first = value
        .get("data")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .ok_or_else(|| {
            AppError::new(
                code::SERVER,
                "服务商没有返回图片（响应里没有 data[0]）".to_string(),
            )
        })?;

    let revised_prompt = first
        .get("revised_prompt")
        .and_then(Value::as_str)
        .map(str::to_string);

    if let Some(b64) = first.get("b64_json").and_then(Value::as_str) {
        if !b64.trim().is_empty() {
            return Ok(ImageResponse {
                payload: ImagePayload::Base64(b64.trim().to_string()),
                revised_prompt,
            });
        }
    }
    for key in ["url", "image"] {
        if let Some(url) = first.get(key).and_then(Value::as_str) {
            let url = url.trim();
            if url.starts_with("http://") || url.starts_with("https://") {
                return Ok(ImageResponse {
                    payload: ImagePayload::Url(url.to_string()),
                    revised_prompt,
                });
            }
            // 有的服务商在这个字段里塞的就是 base64（不带前缀）
            if !url.is_empty() {
                return Ok(ImageResponse {
                    payload: ImagePayload::Base64(url.to_string()),
                    revised_prompt,
                });
            }
        }
    }

    Err(AppError::new(
        code::SERVER,
        "服务商返回的图片数据是空的".to_string(),
    ))
}

/// 这个错误是不是「不认识 response_format」造成的。
///
/// 靠错误文本判断确实不优雅，但服务商在这件事上给的信号只有文本：
/// gpt-image-1 会明确说 `Unknown parameter: 'response_format'`。
/// 所以匹配得很窄（必须同时出现参数名与「unknown/unsupported/invalid」之类的词），
/// 避免把别的 400 也当成「去掉字段再试」而白跑一次。
pub fn looks_like_response_format_rejection(status: u16, body: &str) -> bool {
    if status != 400 && status != 422 {
        return false;
    }
    let lower = body.to_lowercase();
    if !lower.contains("response_format") {
        return false;
    }
    [
        "unknown parameter",
        "unsupported",
        "not supported",
        "invalid",
        "unexpected",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// 从字节的头几个字节判断图片类型。
///
/// 为什么不直接信服务商或 URL 后缀：两者都可能不准，而 `data:` URL 的 mime
/// 一旦写错，WebView 会**静默地什么都不显示** —— 那是最难查的一种表现。
pub fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    None
}

/// 组装 `data:` URL。
pub fn to_data_url(mime: &str, bytes: &[u8]) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    format!("data:{mime};base64,{}", STANDARD.encode(bytes))
}

/// 校验一份拿到的图片字节：非空、不超上限、类型认得出来。
pub fn validate_image_bytes(bytes: &[u8]) -> AppResult<&'static str> {
    if bytes.is_empty() {
        return Err(AppError::new(code::SERVER, "服务商返回了空图片"));
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(AppError::new(
            code::SERVER,
            format!(
                "生成的图片太大了（{:.1} MB，上限 {} MB）",
                bytes.len() as f64 / 1024.0 / 1024.0,
                MAX_IMAGE_BYTES / 1024 / 1024
            ),
        ));
    }
    sniff_image_mime(bytes).ok_or_else(|| {
        AppError::new(
            code::SERVER,
            "返回的内容不是能识别的图片格式（PNG / JPEG / WebP / GIF）".to_string(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_is_built_from_the_base_url() {
        assert_eq!(
            images_url("https://api.openai.com/v1").unwrap(),
            "https://api.openai.com/v1/images/generations"
        );
        // 末尾多一个斜杠也要能收拾
        assert_eq!(
            images_url("https://api.openai.com/v1/").unwrap(),
            "https://api.openai.com/v1/images/generations"
        );
    }

    #[test]
    fn size_is_validated_by_shape_not_by_whitelist() {
        // 各家支持的档位不一样，所以只校验形态
        assert_eq!(validate_size("1024x1024").unwrap(), "1024x1024");
        assert_eq!(validate_size("512x512").unwrap(), "512x512");
        assert_eq!(validate_size("1024x1792").unwrap(), "1024x1792");
        // 前后的空白收拾掉
        assert_eq!(validate_size("  1024x1024  ").unwrap(), "1024x1024");
        // 空值回落默认（这是「没说」而不是「说错了」）
        assert_eq!(validate_size("").unwrap(), DEFAULT_SIZE);
    }

    #[test]
    fn a_malformed_size_is_rejected_rather_than_silently_replaced() {
        // 第一版我写成了「形态不对就回落默认」，但那是错的：用户填 1024X1024
        // （大写 X）时想要的是方形图，静默给他一个默认值等于「要了竖版拿到方的」。
        // 形态不对就明确报错，并把原值回报给他。
        assert!(validate_size("1024X1024").is_err(), "大写 X 也要明确报错");
        for bad in ["1024*1024", "大图", "1024", "1024x", "x1024", "abcxdef"] {
            let err = validate_size(bad).unwrap_err();
            assert!(err.message.contains("宽x高"), "{bad} → {}", err.message);
            // 要把用户填的值原样回报，否则他不知道该改哪里
            assert!(err.message.contains(bad), "{bad} → {}", err.message);
        }
        assert!(validate_size("0x100")
            .unwrap_err()
            .message
            .contains("不能是 0"));
        assert!(validate_size("99999x99999")
            .unwrap_err()
            .message
            .contains("4096"));
    }

    #[test]
    fn body_carries_the_expected_fields() {
        let body = build_image_body("dall-e-3", "一只猫", "1024x1024", true);
        assert_eq!(body["model"], "dall-e-3");
        assert_eq!(body["prompt"], "一只猫");
        assert_eq!(body["n"], 1);
        assert_eq!(body["size"], "1024x1024");
        assert_eq!(body["response_format"], "b64_json");
    }

    #[test]
    fn body_can_drop_response_format() {
        // gpt-image-1 收到这个字段会直接 400，所以必须能不带
        let body = build_image_body("gpt-image-1", "猫", "1024x1024", false);
        assert!(body.get("response_format").is_none());
        assert_eq!(body["model"], "gpt-image-1");
    }

    #[test]
    fn parses_a_base64_response() {
        let parsed = parse_image_response(
            r#"{"created":1,"data":[{"b64_json":"QUJD","revised_prompt":"一只橘猫"}]}"#,
        )
        .unwrap();
        assert_eq!(parsed.payload, ImagePayload::Base64("QUJD".to_string()));
        assert_eq!(parsed.revised_prompt.as_deref(), Some("一只橘猫"));
    }

    #[test]
    fn parses_a_url_response() {
        let parsed =
            parse_image_response(r#"{"data":[{"url":"https://cdn.example.com/a.png"}]}"#).unwrap();
        assert_eq!(
            parsed.payload,
            ImagePayload::Url("https://cdn.example.com/a.png".to_string())
        );
        assert!(parsed.revised_prompt.is_none());
    }

    #[test]
    fn parses_the_alternative_image_field() {
        // 有的兼容层把 base64 放在 data[0].image 里
        let parsed = parse_image_response(r#"{"data":[{"image":"QUJD"}]}"#).unwrap();
        assert_eq!(parsed.payload, ImagePayload::Base64("QUJD".to_string()));
    }

    #[test]
    fn unusable_responses_are_rejected_with_a_clear_reason() {
        for body in [
            "not json at all",
            r#"{"data":[]}"#,
            r#"{"data":[{}]}"#,
            r#"{"data":[{"b64_json":"   "}]}"#,
            r#"{"error":{"message":"没了"}}"#,
        ] {
            let err = parse_image_response(body).unwrap_err();
            assert!(!err.message.is_empty(), "{body} 应当给出原因");
        }
    }

    #[test]
    fn response_format_rejection_is_recognized_narrowly() {
        // 典型：gpt-image-1
        assert!(looks_like_response_format_rejection(
            400,
            r#"{"error":{"message":"Unknown parameter: 'response_format'."}}"#
        ));
        assert!(looks_like_response_format_rejection(
            400,
            r#"{"error":{"message":"response_format is not supported for this model"}}"#
        ));
        // 别的 400 不能被误判（否则会白跑第二次请求）
        assert!(!looks_like_response_format_rejection(
            400,
            r#"{"error":{"message":"content policy violation"}}"#
        ));
        assert!(!looks_like_response_format_rejection(
            400,
            r#"{"error":{"message":"billing hard limit reached"}}"#
        ));
        // 提到字段但状态码不对时也不重试
        assert!(!looks_like_response_format_rejection(
            500,
            "response_format unknown parameter"
        ));
    }

    #[test]
    fn mime_is_sniffed_from_bytes() {
        assert_eq!(
            sniff_image_mime(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00]),
            Some("image/png")
        );
        assert_eq!(
            sniff_image_mime(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some("image/jpeg")
        );
        assert_eq!(sniff_image_mime(b"GIF89a...."), Some("image/gif"));
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBP");
        assert_eq!(sniff_image_mime(&webp), Some("image/webp"));
        // 认不出来就说认不出来，不要瞎猜一个 mime
        assert_eq!(sniff_image_mime(b"<html>error</html>"), None);
    }

    #[test]
    fn bytes_are_validated_before_being_turned_into_a_message() {
        // 空 / 太大 / 不是图片，三种都要拦下来
        assert!(validate_image_bytes(&[])
            .unwrap_err()
            .message
            .contains("空图片"));
        assert!(validate_image_bytes(b"<html>oops</html>")
            .unwrap_err()
            .message
            .contains("不是能识别的图片"));

        let big = vec![0x89; MAX_IMAGE_BYTES + 1];
        let err = validate_image_bytes(&big).unwrap_err();
        assert!(err.message.contains("太大"), "{}", err.message);

        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        assert_eq!(validate_image_bytes(&png).unwrap(), "image/png");
    }

    #[test]
    fn data_url_round_trips() {
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let url = to_data_url("image/png", &png);
        assert!(url.starts_with("data:image/png;base64,"));
        // 前缀之后必须能解回原字节
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine as _;
        let decoded = STANDARD.decode(url.split_once(',').unwrap().1).unwrap();
        assert_eq!(decoded, png);
    }
}
