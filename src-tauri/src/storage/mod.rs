pub mod config;
pub mod database;
pub mod secrets;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 当前 Unix 毫秒时间戳。全项目时间都用 i64 毫秒，避免引入 chrono。
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 生成一个足够唯一的本地 ID。
///
/// 格式：`前缀 + 纳秒时间戳(hex) + 进程内自增序号(hex)`。
/// 单机单进程场景下不需要 UUID 库：时间戳保证跨进程不撞，自增保证同一
/// 纳秒内多次调用不撞。
pub fn new_id(prefix: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!("{prefix}{nanos:x}{n:04x}")
}
