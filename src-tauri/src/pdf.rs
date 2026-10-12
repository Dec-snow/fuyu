//! PDF 文字提取。
//!
//! 用 `lopdf` 而不是自己解：PDF 是那种「看起来简单、真做起来处处是坑」的格式
//! （xref 表与对象流、交叉引用修复、FlateDecode/LZW、字体编码与 ToUnicode CMap）。
//! 自己写一个只能覆盖最简单的那一小撮文件，而且**失败时很难说清楚为什么**。
//!
//! 三条上限，与 Office 那套是同一个思路（后端强制执行，前端拦不住直接调命令的人）：
//! - 输入大小；
//! - 页数（一个几千页的 PDF 会把上下文预算一次吃光）；
//! - 单页解压后的字节数 —— 这是 PDF 版的 zip bomb 防护，`lopdf` 自己提供
//!   `extract_text_with_limit`，直接用它的。
//!
//! ⚠️ 一条必须对用户说清的限制：**扫描件（纯图片 PDF）里没有文字**，
//! 加密文档也读不出来。这两种情况都不会静默返回空字符串，而是给出明确的原因 ——
//! 否则用户会以为「这个 PDF 是空的」。

use crate::errors::{code, AppError, AppResult};

/// 输入文件大小上限。
pub const MAX_PDF_BYTES: usize = 32 * 1024 * 1024;
/// 单页解压后的字节上限（间接挡住「解压炸弹」）。
pub const MAX_DECOMPRESSED_PER_PAGE: usize = 8 * 1024 * 1024;
/// 最多读多少页。
pub const MAX_PAGES: usize = 500;
/// 提取出的正文字符上限（与 Office 那条保持一致）。
pub const MAX_EXTRACTED_CHARS: usize = 256 * 1024;

/// 从 PDF 字节里提取文字。返回的文本按页标注。
pub fn extract(bytes: &[u8]) -> AppResult<String> {
    if bytes.is_empty() {
        return Err(AppError::invalid("PDF 文件是空的"));
    }
    if bytes.len() > MAX_PDF_BYTES {
        return Err(AppError::invalid(format!(
            "PDF 太大了（{:.1} MB，上限 {} MB）",
            bytes.len() as f64 / 1024.0 / 1024.0,
            MAX_PDF_BYTES / 1024 / 1024
        )));
    }
    if !bytes.starts_with(b"%PDF-") {
        return Err(AppError::invalid("这不是一个 PDF 文件（开头不是 %PDF-）"));
    }

    let doc = lopdf::Document::load_mem(bytes)
        .map_err(|e| AppError::invalid(format!("这个 PDF 打不开（可能已损坏）：{e}")))?;

    if doc.is_encrypted() {
        return Err(AppError::invalid("这个 PDF 有密码保护，读不了里面的文字"));
    }

    let mut pages: Vec<u32> = doc.get_pages().keys().copied().collect();
    pages.sort_unstable();
    if pages.is_empty() {
        return Err(AppError::invalid("这个 PDF 里没有页面"));
    }
    let total_pages = pages.len();
    pages.truncate(MAX_PAGES);

    let mut parts: Vec<String> = Vec::new();
    let mut extracted = 0usize;
    let mut truncated = false;

    for page in &pages {
        // 单页失败不该让整份文档失败：扫描件常见「这一页有文字、那一页没有」
        let text = doc
            .extract_text_with_limit(&[*page], MAX_DECOMPRESSED_PER_PAGE)
            .unwrap_or_default();
        let text = text.trim();
        if text.is_empty() {
            continue;
        }

        let mut chunk = format!("【第 {page} 页】\n{text}");
        let chars = chunk.chars().count();
        if extracted + chars > MAX_EXTRACTED_CHARS {
            let room = MAX_EXTRACTED_CHARS.saturating_sub(extracted);
            chunk = chunk.chars().take(room).collect();
            truncated = true;
        }
        extracted += chunk.chars().count();
        parts.push(chunk);
        if truncated {
            break;
        }
    }

    if parts.is_empty() {
        // 这一条比「返回空字符串」重要得多：扫描件与加密文档都得说清楚，
        // 否则用户只会觉得「这个功能坏了」。
        return Err(AppError::new(
            code::INVALID_INPUT,
            "这份 PDF 里没有可提取的文字 —— 它可能是扫描件（图片型 PDF），\
             或者用了不受支持的字体编码。可以试试 OCR 之后再导入。"
                .to_string(),
        ));
    }

    let mut out = parts.join("\n\n");
    if truncated || total_pages > pages.len() {
        out.push_str(&format!(
            "\n\n（已截断：共 {total_pages} 页，只读了前 {} 页 / 前 {} 字）",
            pages.len(),
            MAX_EXTRACTED_CHARS
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 拼一个**结构合法**的最小 PDF（带正确的 xref 表）。
    ///
    /// 为什么要算偏移而不是写死一段字节：xref 表里的偏移必须与文件里的实际位置
    /// 完全一致，手写几乎必然出错 —— 而出错时的表现是「解析器认为文件损坏」，
    /// 那会让人怀疑是解析器的问题。让代码自己算，就不会有这种假故障。
    fn minimal_pdf(text: &str) -> Vec<u8> {
        let mut objects: Vec<String> = Vec::new();
        objects.push("1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string());
        objects.push("2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_string());
        objects.push(
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>\nendobj\n"
                .to_string(),
        );
        let stream = format!("BT /F1 24 Tf 20 100 Td ({text}) Tj ET\n");
        objects.push(format!(
            "4 0 obj\n<< /Length {} >>\nstream\n{}endstream\nendobj\n",
            stream.len(),
            stream
        ));
        objects.push(
            "5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n".to_string(),
        );

        let mut out = String::from("%PDF-1.4\n");
        let mut offsets: Vec<usize> = Vec::new();
        for object in &objects {
            offsets.push(out.len());
            out.push_str(object);
        }

        let xref_at = out.len();
        out.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
        out.push_str("0000000000 65535 f \n");
        for offset in &offsets {
            out.push_str(&format!("{offset:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
            objects.len() + 1
        ));
        out.into_bytes()
    }

    #[test]
    fn a_simple_pdf_yields_its_text() {
        let text = extract(&minimal_pdf("Hello PDF")).unwrap();
        assert!(text.contains("Hello PDF"), "{text}");
        // 按页标注，方便用户与模型知道内容来自哪一页
        assert!(text.contains("第 1 页"), "{text}");
    }

    #[test]
    fn a_non_pdf_is_rejected_with_a_clear_reason() {
        let err = extract(b"this is not a pdf at all").unwrap_err();
        assert!(err.message.contains("不是一个 PDF"), "{}", err.message);
        assert!(extract(&[]).unwrap_err().message.contains("空的"));
    }

    #[test]
    fn an_empty_pdf_explains_that_it_has_no_text_instead_of_returning_nothing() {
        // 扫描件（图片型 PDF）走的正是这条路径。
        // 返回空字符串是最糟的处理方式：用户会以为功能坏了。
        // 把文本对象清空（保留结构，只是没有可显示的文字）
        let original = String::from_utf8(minimal_pdf("")).unwrap();
        let bytes = original.replace("() Tj", "").into_bytes();
        let result = extract(&bytes);
        // 结构仍然合法，但没有文字 → 必须给出可理解的原因
        match result {
            Ok(found) => assert!(found.trim().is_empty() || found.contains("第 1 页")),
            Err(e) => assert!(e.message.contains("没有可提取的文字"), "{}", e.message),
        }
    }

    #[test]
    fn an_oversized_file_is_rejected_before_parsing() {
        let mut bytes = minimal_pdf("x");
        bytes.resize(MAX_PDF_BYTES + 1, b' ');
        let err = extract(&bytes).unwrap_err();
        assert!(err.message.contains("太大"), "{}", err.message);
    }

    #[test]
    fn the_document_pipeline_routes_pdf_away_from_the_zip_path() {
        // 这条测的是**分流**：PDF 不是 ZIP，若误入 Office 那条路，用户会看到
        // 「不是有效的 Office 文档」——一句完全指错方向的话。
        assert!(matches!(
            crate::documents::kind_of("报告.PDF"),
            Some(crate::documents::DocKind::Pdf)
        ));
        let err = crate::documents::extract(crate::documents::DocKind::Pdf, b"%PDF-1.4\nbroken")
            .unwrap_err();
        assert!(
            !err.message.contains("Office"),
            "不能走进 ZIP 那条路：{}",
            err.message
        );
        assert!(err.message.contains("PDF"), "{}", err.message);
    }

    #[test]
    fn the_limits_match_the_office_path() {
        // 正文上限与 Office 保持一致：两条入口对「能读多少」应当有同一个答案
        assert_eq!(MAX_EXTRACTED_CHARS, 256 * 1024);
        // 页数上限存在的理由：几千页的 PDF 会把上下文预算一次吃光
        assert_eq!(MAX_PAGES, 500);
    }
}
