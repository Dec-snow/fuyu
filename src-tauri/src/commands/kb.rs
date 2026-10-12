//! 知识库命令：收录、列出、删除、检索。
//!
//! 收录有两条入口：
//! - `kb_add_text`：前端直接把文字发过来（粘贴的文本、纯文本文件）；
//! - `kb_add_document`：Office 文档（docx / xlsx / pptx）→ 复用
//!   [`crate::commands::documents`] 的提取逻辑 → 再入库。
//!
//! 两条入口最后都走 `Database::kb_add_document`，所以切块与索引只有一份实现。

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use tauri::State;

use crate::documents;
use crate::errors::{AppError, AppResult};
use crate::kb::{KbDocument, KbHit, KbStats, DEFAULT_SEARCH_LIMIT, MAX_SEARCH_LIMIT};
use crate::storage::database::Database;

#[tauri::command]
pub async fn kb_list(db: State<'_, Database>) -> AppResult<Vec<KbDocument>> {
    db.kb_list_documents()
}

#[tauri::command]
pub async fn kb_stats(db: State<'_, Database>) -> AppResult<KbStats> {
    db.kb_stats()
}

/// 收一段文字（粘贴的文本、或前端读出来的纯文本文件）。
#[tauri::command]
pub async fn kb_add_text(
    db: State<'_, Database>,
    title: String,
    source: Option<String>,
    text: String,
) -> AppResult<KbDocument> {
    db.kb_add_document(&title, source.as_deref().unwrap_or(""), &text)
}

/// 收一个 Office 文档：先提取文字，再入库。
#[tauri::command]
pub async fn kb_add_document(
    db: State<'_, Database>,
    file_name: String,
    payload: String,
) -> AppResult<KbDocument> {
    let kind = documents::kind_of(&file_name).ok_or_else(|| {
        AppError::invalid(format!(
            "「{file_name}」不能收进知识库（只支持 docx / xlsx / pptx，纯文本请用粘贴或拖入）"
        ))
    })?;

    let body = payload
        .split_once("base64,")
        .filter(|(head, _)| head.starts_with("data:"))
        .map(|(_, rest)| rest)
        .unwrap_or(payload.as_str())
        .trim();

    let bytes = STANDARD
        .decode(body)
        .map_err(|e| AppError::invalid(format!("文件内容不是有效的 base64：{e}")))?;
    let text = documents::extract(kind, &bytes)?;

    // 标题去掉扩展名，列表里更好读
    let title = file_name
        .rsplit_once('.')
        .map(|(head, _)| head.to_string())
        .unwrap_or_else(|| file_name.clone());
    db.kb_add_document(&title, &file_name, &text)
}

#[tauri::command]
pub async fn kb_delete(db: State<'_, Database>, id: String) -> AppResult<()> {
    db.kb_delete_document(&id)
}

#[tauri::command]
pub async fn kb_search(
    db: State<'_, Database>,
    query: String,
    limit: Option<usize>,
) -> AppResult<Vec<KbHit>> {
    let limit = limit.unwrap_or(DEFAULT_SEARCH_LIMIT).min(MAX_SEARCH_LIMIT);
    db.kb_search(&query, limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pasted_text_has_no_extension_requirement() {
        // 纯文本入口不做扩展名判断（前端已经限定了来源）
        assert_eq!(documents::kind_of("读书笔记"), None);
        assert_eq!(documents::kind_of("读书笔记.txt"), None);
        assert_eq!(
            documents::kind_of("报告.docx"),
            Some(documents::DocKind::Docx)
        );
    }
}
