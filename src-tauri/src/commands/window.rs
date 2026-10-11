//! 窗口相关命令。
//!
//! 前端**不**直接调用 `@tauri-apps/api/window` 的 show/hide/setSize，
//! 而是调这些命令。好处：权限面收敛到 `core:default`，位置记忆、动画兜底、
//! 模式持久化这些逻辑都留在 Rust 侧，前端只管渲染。

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, State};

use crate::errors::{AppError, AppResult};
use crate::storage::config::ConfigStore;
use crate::window::{manager, shortcuts};

/// 退场动画是否还在等前端回报。
///
/// 前端播完动画会调 `finish_hide` 把它换回 `false`；`manager` 里的兜底线程
/// 若发现它还是 `true` 就强制隐藏。
pub static HIDE_PENDING: AtomicBool = AtomicBool::new(false);

#[tauri::command]
pub fn show_window(app: AppHandle) {
    if let Some(window) = manager::main_window(&app) {
        manager::show_and_focus(&window);
    }
}

#[tauri::command]
pub fn hide_window(app: AppHandle) {
    if let Some(window) = manager::main_window(&app) {
        manager::save_position(&window);
        manager::hide_with_animation(&window);
    }
}

/// 前端退场动画播完后调用，取消兜底强制隐藏。
#[tauri::command]
pub fn finish_hide(app: AppHandle) {
    HIDE_PENDING.store(false, Ordering::SeqCst);
    if let Some(window) = manager::main_window(&app) {
        let _ = window.hide();
    }
}

#[tauri::command]
pub fn toggle_window(app: AppHandle) {
    manager::toggle(&app);
}

/// 悬浮模式 ↔ 完整模式。
#[tauri::command]
pub fn set_window_mode(
    app: AppHandle,
    config: State<'_, ConfigStore>,
    expanded: bool,
) -> AppResult<()> {
    config.update(|cfg| {
        cfg.expanded = expanded;
        Ok(())
    })?;

    if let Some(window) = manager::main_window(&app) {
        let cfg = config.read(|c| c.clone());
        manager::apply_mode(&window, expanded, &cfg)
            .map_err(|e| AppError::internal(format!("切换窗口模式失败：{e}")))?;
    }
    Ok(())
}

/// 用户拖动窗口结束时前端调用一次，把位置记下来。
#[tauri::command]
pub fn save_window_position(app: AppHandle) -> AppResult<()> {
    if let Some(window) = manager::main_window(&app) {
        manager::save_position(&window);
    }
    Ok(())
}

/// 从配置重新注册两组全局快捷键（唤醒 + 划词）。
///
/// 设置里改动「是否启用划词」后调用，让它立刻生效而不用重启。
#[tauri::command]
pub fn reapply_shortcuts(app: AppHandle) {
    let _ = crate::window::shortcuts::register(&app);
}

#[tauri::command]
pub fn get_shortcut(config: State<'_, ConfigStore>) -> String {
    config.read(|cfg| {
        if cfg.shortcut.trim().is_empty() {
            shortcuts::DEFAULT_SHORTCUT.to_string()
        } else {
            cfg.shortcut.clone()
        }
    })
}

/// 改全局快捷键。
///
/// **先注册再落盘**：注册失败时配置保持不变，用户看到报错后重开应用仍然是
/// 原来能用的那个键，而不会进入「配置里写着一个抢不到的键」的死状态。
#[tauri::command]
pub fn set_shortcut(
    app: AppHandle,
    config: State<'_, ConfigStore>,
    shortcut: String,
) -> AppResult<String> {
    let shortcut = shortcut.trim().to_string();
    shortcuts::register_value(&app, &shortcut).map_err(AppError::invalid)?;
    config.update(|cfg| {
        cfg.shortcut = shortcut.clone();
        Ok(())
    })?;
    Ok(shortcut)
}

/// 改划词助手的热键（`Alt+Q` 默认值常被其他程序占用，所以要能改）。
///
/// 与 [`set_shortcut`] 一样**先注册再落盘**。
#[tauri::command]
pub fn set_quick_action_shortcut(
    app: AppHandle,
    config: State<'_, ConfigStore>,
    shortcut: String,
) -> AppResult<String> {
    let shortcut = shortcut.trim().to_string();
    if shortcut.is_empty() {
        return Err(AppError::invalid("划词快捷键不能为空"));
    }
    shortcuts::register_quick_action_value(&app, &shortcut).map_err(AppError::invalid)?;
    config.update(|cfg| {
        cfg.quick_action_shortcut = shortcut.clone();
        Ok(())
    })?;
    Ok(shortcut)
}
