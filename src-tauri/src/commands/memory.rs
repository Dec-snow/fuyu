//! 长期记忆命令：列出、手动添加、删除。
//!
//! 模型自己记（`remember` 工具）与用户手动记走的是**同一张表、同一套去重逻辑**
//! —— 两条入口只有 `source` 不同（`tool` / `user`）。分开两套逻辑迟早会出现
//! 「手动加的不会被去重」这种差异。

use serde::Serialize;
use tauri::State;

use crate::errors::AppResult;
use crate::memory::{Memory, MemoryAddOutcome};
use crate::storage::database::Database;

/// 记忆页要显示的概览。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryStats {
    pub total: i64,
    /// 上限（界面显示「12 / 500」，让用户知道还能加多少）。
    pub limit: i64,
    /// 被用过至少一次的有几条 —— 用来判断「记的东西到底有没有用」。
    pub used: i64,
}

#[tauri::command]
pub async fn memory_list(db: State<'_, Database>) -> AppResult<Vec<Memory>> {
    db.memory_list()
}

#[tauri::command]
pub async fn memory_stats(db: State<'_, Database>) -> AppResult<MemoryStats> {
    let list = db.memory_list()?;
    Ok(MemoryStats {
        total: list.len() as i64,
        limit: crate::memory::MAX_MEMORIES,
        used: list.iter().filter(|m| m.used_count > 0).count() as i64,
    })
}

#[tauri::command]
pub async fn memory_add(db: State<'_, Database>, text: String) -> AppResult<MemoryAddOutcome> {
    db.memory_add(&text, "user")
}

#[tauri::command]
pub async fn memory_delete(db: State<'_, Database>, id: String) -> AppResult<()> {
    db.memory_delete(&id)
}

/// 试搜：看看某句话会挑中哪几条记忆。
///
/// 存在的理由与知识库那个试搜框一样：注入是「看不见的行为」，
/// 用户没法确认它到底会不会把对的记忆带上。给一个能亲眼看到的地方。
#[tauri::command]
pub async fn memory_relevant(db: State<'_, Database>, query: String) -> AppResult<Vec<Memory>> {
    db.memory_relevant(&query, crate::memory::INJECT_LIMIT)
}
