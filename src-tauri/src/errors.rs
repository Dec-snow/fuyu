//! 统一错误类型。
//!
//! 参考项目在命令边界统一返回 `Result<T, String>`，字符串直接给用户看；
//! 这里做得更结构化一点：`AppError` 自带 `code` / `message` / `retryable`
//! 三个字段并实现 `Serialize`，Tauri 会把它原样序列化给前端，前端可以按
//! `code` 分类处理（例如 401 → 跳设置页，429 → 允许重试）。
//!
//! 安全约定：任何 `Authorization` 头、API Key、请求体都不得出现在
//! `message` 里，也不得写日志。

use std::fmt;

use serde::{Deserialize, Serialize};

/// 稳定错误码，前端按它分支，不依赖 message 文案。
pub mod code {
    pub const INVALID_INPUT: &str = "invalid_input";
    pub const SECRET: &str = "secret";
    pub const DB: &str = "db";
    pub const NETWORK: &str = "network";
    pub const UNAUTHORIZED: &str = "unauthorized";
    pub const RATE_LIMITED: &str = "rate_limited";
    pub const NOT_FOUND: &str = "not_found";
    pub const SERVER: &str = "server";
    pub const CANCELLED: &str = "cancelled";
    pub const TIMEOUT: &str = "timeout";
    pub const INTERNAL: &str = "internal";
}

/// 序列化成 `{code, message, retryable}`（`camelCase` 由外层容器决定）。
///
/// 需要 `Deserialize`：这个错误会被写进 SQLite 的消息表（失败的那一轮），
/// 下次打开会话再读回来显示。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl AppError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            retryable: false,
        }
    }

    pub fn retryable(mut self, yes: bool) -> Self {
        self.retryable = yes;
        self
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(code::INVALID_INPUT, message)
    }

    pub fn secret(message: impl Into<String>) -> Self {
        Self::new(code::SECRET, message)
    }

    pub fn db(message: impl Into<String>) -> Self {
        Self::new(code::DB, message)
    }

    pub fn network(message: impl Into<String>) -> Self {
        Self::new(code::NETWORK, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(code::INTERNAL, message)
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for AppError {}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        AppError::db(format!("数据库错误：{e}"))
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::internal(format!("文件读写失败：{e}"))
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::internal(format!("JSON 解析失败：{e}"))
    }
}

impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            return AppError::new(code::TIMEOUT, "请求超时，请检查网络或稍后重试").retryable(true);
        }
        if e.is_connect() {
            return AppError::network(format!("无法连接到服务商：{}", brief(&e))).retryable(true);
        }
        AppError::network(brief(&e)).retryable(true)
    }
}

pub type AppResult<T> = Result<T, AppError>;

/// 截断 reqwest 的长错误串，避免把内部 URL / 细节整段抛给用户。
pub fn brief(err: &reqwest::Error) -> String {
    let mut s = err.to_string();
    if s.chars().count() > 200 {
        s = s.chars().take(200).collect::<String>() + "…";
    }
    s
}

/// 带上限地读取响应体，防止恶意/异常服务端把内存吃光。
///
/// 先看 `Content-Length` 做廉价预检，再逐块读取并累计，任一步超限即报错。
pub async fn read_body_capped(resp: reqwest::Response, max_bytes: usize) -> AppResult<String> {
    use futures_util::StreamExt;

    if let Some(len) = resp.content_length() {
        if len as usize > max_bytes {
            return Err(AppError::new(
                code::SERVER,
                format!("响应内容过大（超过 {} KB）", max_bytes / 1024),
            ));
        }
    }
    let mut body: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(AppError::from)?;
        if body.len() + bytes.len() > max_bytes {
            return Err(AppError::new(
                code::SERVER,
                format!("响应内容过大（超过 {} KB）", max_bytes / 1024),
            ));
        }
        body.extend_from_slice(&bytes);
    }
    String::from_utf8(body).map_err(|_| AppError::new(code::SERVER, "响应不是有效的 UTF-8 文本"))
}
