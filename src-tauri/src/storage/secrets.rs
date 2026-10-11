//! API Key 存放于 Windows 凭据管理器（`keyring`）。
//!
//! 三条纪律：
//! 1. Key **只**在 Rust 侧读取，用来拼 `Authorization` 头；
//! 2. 永远不序列化给前端（前端只能拿到 `hasKey: bool`）；
//! 3. 永远不写进日志或错误消息。

use keyring::Entry;

const SERVICE: &str = "FloatAI";

fn entry(provider_id: &str) -> Result<Entry, String> {
    Entry::new(SERVICE, &format!("provider:{provider_id}"))
        .map_err(|e| format!("无法访问系统凭据库：{e}"))
}

/// 写入（`key` 为空则删除）。
pub fn set_api_key(provider_id: &str, key: &str) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() {
        return delete_api_key(provider_id);
    }
    entry(provider_id)?
        .set_password(key)
        .map_err(|e| format!("写入 API Key 失败：{e}"))
}

/// `Ok(None)` 表示“该服务商还没配 Key”，不是错误。
pub fn get_api_key(provider_id: &str) -> Result<Option<String>, String> {
    match entry(provider_id)?.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("读取 API Key 失败：{e}")),
    }
}

pub fn delete_api_key(provider_id: &str) -> Result<(), String> {
    match entry(provider_id)?.delete_credential() {
        Ok(()) => Ok(()),
        // 本来就不存在 → 视为成功，保证删除是幂等的。
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("删除 API Key 失败：{e}")),
    }
}

/// 前端只问“有没有 Key”，因此不需要把 Key 本身传出去。
pub fn has_api_key(provider_id: &str) -> bool {
    matches!(get_api_key(provider_id), Ok(Some(k)) if !k.trim().is_empty())
}
