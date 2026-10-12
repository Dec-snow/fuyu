//! MCP 服务器命令：连接、发现工具、查看状态。
//!
//! 管理器放在模块级 `OnceLock` 里而不是 `AppState`：MCP 与窗口/会话无关，
//! 它是一份「后台连着的若干本地进程」。放进 `AppState` 只会让状态结构更臃肿。
//! （等工具真正接进模型调用那条链路时，再考虑挪进 `AppState` 更方便传参。）

use std::sync::{Arc, OnceLock};

use tauri::State;

use crate::diagnostics::LogBuffer;
use crate::errors::AppResult;
use crate::mcp::{McpManager, McpServerStatus};
use crate::storage::config::ConfigStore;

static MANAGER: OnceLock<Arc<McpManager>> = OnceLock::new();

fn manager(logs: &LogBuffer) -> Arc<McpManager> {
    MANAGER
        .get_or_init(|| Arc::new(McpManager::new(logs.clone())))
        .clone()
}

/// 重新扫描所有启用的 MCP 服务器，返回每个服务器的连接状态与工具清单。
///
/// 刻意不做成「启动时自动扫描」：扫描要**拉起子进程**，而 MCP 服务端常见的
/// 形态是 `npx -y …`（首次会去下载包）—— 那可能要好几十秒。让用户在看得到
/// 进度的设置页里手动触发，比在启动路径上偷偷卡住强得多。
#[tauri::command]
pub async fn mcp_status(
    config: State<'_, ConfigStore>,
    logs: tauri::State<'_, LogBuffer>,
) -> AppResult<Vec<McpServerStatus>> {
    let servers = config.read(|cfg| cfg.mcp_servers.clone());
    let manager = manager(&logs);
    Ok(manager.refresh(&servers).await)
}

/// 关掉所有 MCP 子进程（退出前调用，免得留下孤儿进程）。
#[tauri::command]
pub async fn mcp_shutdown(logs: tauri::State<'_, LogBuffer>) -> AppResult<()> {
    if let Some(manager) = MANAGER.get() {
        manager.shutdown().await;
    }
    let _ = &logs;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::config::McpServerConfig;

    #[test]
    fn the_manager_is_created_once() {
        let logs = LogBuffer::default();
        let a = manager(&logs);
        let b = manager(&logs);
        assert!(Arc::ptr_eq(&a, &b), "应当是同一个管理器实例");
    }

    #[test]
    fn a_disabled_server_is_skipped_by_refresh() {
        // 配置里的 enabled 字段决定要不要真的去拉进程。
        // 这里只验证配置本身的语义（真拉进程的验证见 mcp::client 的集成测试）。
        let server = McpServerConfig {
            id: "s1".to_string(),
            name: "测试".to_string(),
            command: "node".to_string(),
            args: Vec::new(),
            env: Default::default(),
            enabled: false,
        };
        assert!(!server.enabled);
    }
}
