//! 文档解析命令：把 Office 文档里的文字提出来，交给前端当附件用。
//!
//! 为什么解析放在 Rust 侧而不是前端：docx / xlsx / pptx 是 ZIP + XML，
//! 前端要解就得引一堆 JS 库（且体积极大）。Rust 这边 `zip` + `quick-xml`
//! 已经够了，而且**大小与体积上限能强制执行**（zip bomb 防护在前端做没有意义，
//! 因为前端的限制拦不住直接调命令的人）。
//!
//! 传输格式：前端把文件读成 data URL 再发过来（与图片那条路一致），
//! 这里两种都收：完整的 `data:...;base64,xxx`，或裸 base64。

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde::Serialize;

use crate::documents::{self, DocKind};
use crate::errors::{AppError, AppResult};

/// 允许的 base64 载荷长度上限（约 9 MB 原始文件）。
///
/// 比文本附件的 256 KB 宽松得多 —— Office 文档本来就大（一张图就能几 MB），
/// 而提取出来的**正文**另有 256 KB 的上限兜着。
pub const MAX_PAYLOAD_CHARS: usize = 12 * 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractedDocument {
    pub file_name: String,
    /// 人类可读的类型名（「Word 文档」等），用于提示与附件块的标题。
    pub kind: String,
    pub text: String,
    /// 提取出的字符数。
    pub chars: usize,
    /// 是否因为过长被截断（界面要如实告诉用户）。
    pub truncated: bool,
}

/// 去掉 data URL 前缀，取出 base64 主体。
fn strip_data_url(payload: &str) -> &str {
    match payload.split_once("base64,") {
        // 只在**确实**是 data URL 时才切：否则 base64 里出现的 "base64,"
        // 这种巧合会把内容截断（虽然极不可能，但判据要写清楚）
        Some((head, rest)) if head.starts_with("data:") => rest,
        _ => payload,
    }
}

/// 解析一个 Office 文档，返回其中的文字。
#[tauri::command]
pub async fn extract_document(file_name: String, payload: String) -> AppResult<ExtractedDocument> {
    let kind: DocKind = documents::kind_of(&file_name).ok_or_else(|| {
        AppError::invalid(format!(
            "「{file_name}」不是支持的文档类型（只支持 docx / xlsx / pptx）"
        ))
    })?;

    let body = strip_data_url(&payload).trim();
    if body.is_empty() {
        return Err(AppError::invalid("文件内容为空"));
    }
    if body.len() > MAX_PAYLOAD_CHARS {
        return Err(AppError::invalid(format!(
            "文件太大（{:.1} MB，上限约 {} MB）",
            body.len() as f64 / 1_048_576.0 * 0.75,
            (MAX_PAYLOAD_CHARS as f64 * 0.75 / 1_048_576.0).round()
        )));
    }

    let bytes = STANDARD
        .decode(body)
        .map_err(|e| AppError::invalid(format!("文件内容不是有效的 base64：{e}")))?;

    let text = documents::extract(kind, &bytes)?;
    let chars = text.chars().count();
    Ok(ExtractedDocument {
        file_name,
        kind: kind.label().to_string(),
        truncated: text.contains("已截断到"),
        text,
        chars,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_url_prefix_is_stripped() {
        assert_eq!(strip_data_url("data:application/x;base64,AAAA"), "AAAA");
        assert_eq!(strip_data_url("AAAA"), "AAAA");
        // 不是 data URL 时不能乱切
        assert_eq!(strip_data_url("base64,AAAA"), "base64,AAAA");
    }

    #[tokio::test]
    async fn an_unsupported_extension_is_rejected() {
        // 用 .doc（老格式）当例子：pdf 现在**是**支持的，拿它当反例会让这条测试
        // 变成「断言错误的东西」—— 它之前正是这样，被 pdf 的接入打回了。
        let err = extract_document("报告.doc".to_string(), "AAAA".to_string())
            .await
            .unwrap_err();
        assert!(
            err.message.contains("不是支持的文档类型"),
            "{}",
            err.message
        );
    }

    #[tokio::test]
    async fn an_empty_payload_is_rejected() {
        let err = extract_document("a.docx".to_string(), "   ".to_string())
            .await
            .unwrap_err();
        assert!(err.message.contains("为空"), "{}", err.message);
    }

    #[tokio::test]
    async fn invalid_base64_is_rejected_before_parsing() {
        let err = extract_document("a.docx".to_string(), "这不是 base64!!!".to_string())
            .await
            .unwrap_err();
        assert!(err.message.contains("base64"), "{}", err.message);
    }

    #[tokio::test]
    async fn an_oversized_payload_is_rejected() {
        let huge = "A".repeat(MAX_PAYLOAD_CHARS + 4);
        let err = extract_document("a.docx".to_string(), huge)
            .await
            .unwrap_err();
        assert!(err.message.contains("太大"), "{}", err.message);
    }

    #[tokio::test]
    async fn a_real_docx_round_trips_through_the_whole_command() {
        // 端到端：真夹具 → data URL → 真命令 → 提取结果。
        // 单测覆盖的是各段逻辑，这条覆盖它们串起来的样子（含 base64 解码、
        // data URL 前缀剥离、类型名回填）。
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("sample.docx");
        let bytes = std::fs::read(&path).expect("读不到夹具，先跑 tools/make-office-fixtures.py");
        let data_url = format!(
            "data:application/vnd.openxmlformats-officedocument.wordprocessingml.document;base64,{}",
            STANDARD.encode(&bytes)
        );

        let out = extract_document("季度报告.docx".to_string(), data_url)
            .await
            .unwrap();
        assert_eq!(out.file_name, "季度报告.docx");
        assert_eq!(out.kind, "Word 文档");
        assert!(!out.truncated);
        assert!(out.chars > 20);
        assert!(
            out.text.contains("浮语文档解析测试"),
            "提取结果不对：{}",
            out.text
        );
        assert!(out.text.contains("单元格二"), "表格内容要一起提出来");
    }

    #[tokio::test]
    async fn base64_that_is_not_a_zip_is_rejected() {
        // 合法 base64，内容却不是压缩包
        let payload = STANDARD.encode(b"just some text, not a zip");
        let err = extract_document("a.docx".to_string(), payload)
            .await
            .unwrap_err();
        assert!(
            err.message.contains("不是有效的 Office 文档"),
            "{}",
            err.message
        );
    }
}
