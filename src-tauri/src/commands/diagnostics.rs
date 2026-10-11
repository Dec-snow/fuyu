//! 诊断信息与数据管理。
//!
//! 两件事放在一起，因为它们都属于「这台机器上的东西出了状况，用户需要看/需要清」：
//! - `diagnostics`：把系统信息、存储状态、最近的错误汇总成一份可复制的文本；
//! - `reset_settings` / `delete_all_api_keys`：把设置和密钥清干净。

use std::path::PathBuf;

use serde::Serialize;
use tauri::State;

use crate::diagnostics::LogEntry;
use crate::errors::AppResult;
use crate::state::AppState;
use crate::storage::config::{AppConfig, ConfigStore};
use crate::storage::database::Database;
use crate::storage::secrets;

/// 诊断页要显示的全部内容。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostics {
    pub version: String,
    pub os: String,
    pub arch: String,
    pub db_path: String,
    /// 库文件与 `-wal` / `-shm` 的合计（WAL 模式下只看主文件会明显偏小）。
    pub db_bytes: u64,
    pub db_version: i64,
    pub conversations: i64,
    pub messages: i64,
    pub config_path: String,
    pub config_bytes: u64,
    pub providers: usize,
    /// 联网搜索是否配了端点（配了才有 `web_search` 工具）。
    pub web_search_configured: bool,
    pub always_allowed_tools: Vec<String>,
    pub logs: Vec<LogEntry>,
}

/// 每个文件都算上：SQLite 的 WAL 模式下，刚写完的数据可能还在 `-wal` 里。
fn file_size(path: &std::path::Path) -> u64 {
    let mut total = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    for suffix in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{suffix}", path.display()));
        total += std::fs::metadata(side).map(|m| m.len()).unwrap_or(0);
    }
    total
}

/// 汇总诊断信息（最近的错误日志取最后 50 条）。
#[tauri::command]
pub async fn diagnostics(
    state: State<'_, AppState>,
    db: State<'_, Database>,
    config: State<'_, ConfigStore>,
) -> AppResult<Diagnostics> {
    let (conversations, messages) = db.counts()?;
    let cfg = config.read(|c| c.clone());

    Ok(Diagnostics {
        version: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        db_path: db.path().display().to_string(),
        db_bytes: file_size(db.path()),
        db_version: db.schema_version()?,
        conversations,
        messages,
        config_path: config.path().display().to_string(),
        config_bytes: file_size(config.path()),
        providers: cfg.providers.len(),
        web_search_configured: cfg
            .web_search_endpoint
            .as_deref()
            .is_some_and(|e| !e.trim().is_empty()),
        always_allowed_tools: cfg.always_allowed_tools.clone(),
        logs: state.logs.recent(50),
    })
}

/// 清空诊断日志，返回清掉了几条（界面用它给一句反馈）。
#[tauri::command]
pub async fn clear_logs(state: State<'_, AppState>) -> AppResult<usize> {
    let removed = state.logs.len();
    state.logs.clear();
    Ok(removed)
}

/// 「重置设置」时保留哪些字段。
///
/// 只保留**服务商相关内容**：服务商列表、默认服务商、默认模型。
/// 理由不是偷懒 —— API Key 存在系统凭据管理器里，是按 provider id 挂的。
/// 如果把服务商列表也重置掉，那些密钥就变成**没有任何界面能看到的孤儿**：
/// 用户以为「重置设置」只是恢复默认外观，结果密钥永远留在系统里删不掉。
///
/// 其余一律回到默认值：外观、提示词、人设（回到内置模板）、工具白名单、
/// 代理、温度、上下文预算……
fn reset_keeping_providers(cfg: &mut AppConfig) {
    let providers = std::mem::take(&mut cfg.providers);
    let default_provider_id = cfg.default_provider_id.take();
    let default_model_key = cfg.default_model_key.take();
    *cfg = AppConfig {
        providers,
        default_provider_id,
        default_model_key,
        ..AppConfig::default()
    };
}

/// 重置全部设置（保留服务商与密钥）。
#[tauri::command]
pub async fn reset_settings(config: State<'_, ConfigStore>) -> AppResult<AppConfig> {
    config.update(|cfg| {
        reset_keeping_providers(cfg);
        // 返回重置后的整份配置：前端拿它直接刷新界面，不用再读一次。
        Ok(cfg.clone())
    })
}

/// 删除**所有**服务商的 API Key。
///
/// 单独一个动作，不藏在「重置设置」里：这是不可撤销的（密钥删了就没了），
/// 而且用户想「把密钥从这台机器上撤掉」时不该被迫连外观设置一起重来。
#[tauri::command]
pub async fn delete_all_api_keys(config: State<'_, ConfigStore>) -> AppResult<usize> {
    let ids: Vec<String> = config.read(|c| c.providers.iter().map(|p| p.id.clone()).collect());

    let mut removed = 0;
    for id in ids {
        // 某个服务商没有密钥会返回 Ok（幂等），真正失败时记一条日志并继续 ——
        // 一个密钥删不掉不该让整批停下，用户至少能知道删了几个。
        match secrets::delete_api_key(&id) {
            Ok(()) => removed += 1,
            Err(e) => eprintln!("删除密钥失败（{id}）：{e}"),
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resetting_settings_keeps_the_providers_but_restores_everything_else() {
        let mut cfg = AppConfig::seeded();
        // 先把「用户改过的东西」摆上去
        cfg.theme = "dark".to_string();
        cfg.system_prompt = "我自己的提示词".to_string();
        cfg.window_opacity = 0.5;
        cfg.message_font_size = 18.0;
        cfg.temperature = 1.5;
        cfg.always_allowed_tools = vec!["read_clipboard_text".to_string()];
        cfg.roles = vec![crate::storage::config::Role {
            id: "mine".to_string(),
            name: "我的角色".to_string(),
            prompt: "p".to_string(),
            builtin: false,
        }];
        let providers_before = cfg.providers.len();
        let default_before = cfg.default_provider_id.clone();

        reset_keeping_providers(&mut cfg);

        // 保留：服务商相关
        assert_eq!(cfg.providers.len(), providers_before, "服务商不能被重置掉");
        assert_eq!(cfg.default_provider_id, default_before);

        // 恢复默认：其余一切
        let defaults = AppConfig::default();
        assert_eq!(cfg.theme, defaults.theme);
        assert_eq!(cfg.system_prompt, defaults.system_prompt);
        assert_eq!(cfg.window_opacity, defaults.window_opacity);
        assert_eq!(cfg.message_font_size, defaults.message_font_size);
        assert_eq!(cfg.temperature, defaults.temperature);
        assert!(cfg.always_allowed_tools.is_empty(), "工具白名单要清掉");
        assert_eq!(
            cfg.roles.len(),
            crate::storage::config::Role::presets().len(),
            "人设回到内置模板"
        );
        assert!(cfg.roles.iter().all(|r| r.builtin));
    }

    #[test]
    fn resetting_an_empty_config_is_harmless() {
        // 用户可能在没有服务商的状态下点「重置设置」（还没配过）。
        let mut cfg = AppConfig::default();
        reset_keeping_providers(&mut cfg);
        assert!(cfg.providers.is_empty());
    }

    #[test]
    fn file_size_tolerates_a_missing_file() {
        // 关键：诊断页不能因为某个文件不存在就整页报错 ——
        // 那恰恰是用户最需要看到诊断信息的时候。
        let missing = std::path::Path::new("Z:/definitely/not/here.db");
        assert_eq!(file_size(missing), 0);
    }
}
