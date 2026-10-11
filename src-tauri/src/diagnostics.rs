//! 运行期诊断：一段**有界的**错误日志 + 系统与存储信息汇总。
//!
//! 为什么需要它：这个应用出问题时（请求失败、设置写不进去），错误只出现在
//! 界面上那一条提示里 —— 用户一刷新就没了，想反馈也说不清。以前唯一能看到
//! 细节的地方是「从命令行启动 exe 看 stderr」，这对普通用户等于不存在。
//!
//! 两条设计约束：
//! - **有界**：只留最近 [`MAX_LOG_ENTRIES`] 条。诊断日志不能自己变成内存泄漏源。
//! - **不记敏感内容**：这里只记错误码与错误信息，**绝不记 API Key、请求正文、
//!   消息内容**。诊断信息是要给用户复制粘贴给人看的，天然是外发渠道。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde::Serialize;

/// 保留的日志条数上限。
pub const MAX_LOG_ENTRIES: usize = 200;

/// 一条诊断记录。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub at: i64,
    /// 来源：`chat` / `config` / `provider` / `db` / `tool`。
    pub source: String,
    pub message: String,
}

/// 有界的诊断日志。克隆它只是复制一个 `Arc`（多个任务要往里写）。
#[derive(Debug, Default, Clone)]
pub struct LogBuffer {
    inner: Arc<Mutex<VecDeque<LogEntry>>>,
}

impl LogBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记一条。满了就丢最旧的 —— 保留「最近发生了什么」比保留陈年旧账有用。
    pub fn record(&self, source: &str, message: impl Into<String>) {
        let entry = LogEntry {
            at: crate::storage::now_ms(),
            source: source.to_string(),
            message: message.into(),
        };
        let mut log = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if log.len() >= MAX_LOG_ENTRIES {
            log.pop_front();
        }
        log.push_back(entry);
    }

    /// 最近的若干条，**最新在前**（界面直接从上往下渲染）。
    pub fn recent(&self, limit: usize) -> Vec<LogEntry> {
        let log = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        log.iter().rev().take(limit).cloned().collect()
    }

    pub fn clear(&self) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_come_back_newest_first() {
        let log = LogBuffer::new();
        log.record("chat", "第一条");
        log.record("config", "第二条");
        log.record("db", "第三条");

        let recent = log.recent(10);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].message, "第三条");
        assert_eq!(recent[2].message, "第一条");
        assert_eq!(recent[1].source, "config");
    }

    #[test]
    fn the_buffer_is_bounded_and_drops_the_oldest() {
        // 诊断日志绝不能自己变成内存泄漏源：跑一整天之后它必须还是有限大小。
        let log = LogBuffer::new();
        for i in 0..(MAX_LOG_ENTRIES + 50) {
            log.record("chat", format!("第 {i} 条"));
        }
        assert_eq!(log.len(), MAX_LOG_ENTRIES);
        let recent = log.recent(MAX_LOG_ENTRIES);
        assert_eq!(recent[0].message, format!("第 {} 条", MAX_LOG_ENTRIES + 49));
        // 最旧的那些确实被丢掉了
        assert!(recent.iter().all(|e| e.message != "第 0 条"));
    }

    #[test]
    fn recent_respects_the_limit() {
        let log = LogBuffer::new();
        for i in 0..10 {
            log.record("chat", format!("{i}"));
        }
        assert_eq!(log.recent(3).len(), 3);
        assert_eq!(log.recent(0).len(), 0);
    }

    #[test]
    fn clearing_empties_it() {
        let log = LogBuffer::new();
        log.record("chat", "x");
        assert_eq!(log.len(), 1);
        log.clear();
        assert_eq!(log.len(), 0);
        assert!(log.recent(10).is_empty());
    }

    #[test]
    fn clones_share_the_same_buffer() {
        // 多个后台任务各持一份克隆往里写 —— 必须是同一个缓冲区，
        // 否则诊断页只能看到最后初始化的那个任务写的日志。
        let log = LogBuffer::new();
        let other = log.clone();
        other.record("chat", "来自克隆");
        assert_eq!(log.recent(1)[0].message, "来自克隆");
    }
}
