//! 用量与费用：按天聚合 token，并按内置价格表估算花费。

use tauri::State;

use crate::errors::AppResult;
use crate::models::pricing::{self, UsageReport};
use crate::storage::database::Database;
use crate::storage::now_ms;

/// 默认统计最近多少天。
pub const DEFAULT_USAGE_DAYS: u32 = 30;
/// 上限。给「全部时间」留了足够空间，同时挡住 `u32::MAX` 这种输入
/// （那会让 `now - days * 86400_000` 溢出）。
pub const MAX_USAGE_DAYS: u32 = 3650;

/// 最近 `days` 天的用量与估算花费。
#[tauri::command]
pub async fn usage_report(db: State<'_, Database>, days: Option<u32>) -> AppResult<UsageReport> {
    let days = days.unwrap_or(DEFAULT_USAGE_DAYS).clamp(1, MAX_USAGE_DAYS);
    let since = now_ms() - (days as i64) * 86_400_000;
    let rows = db.usage_by_day_model(since)?;
    Ok(pricing::summarize(&rows))
}
