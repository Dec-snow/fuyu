//! 开机自启。
//!
//! 用 Tauri 官方的 `tauri-plugin-autostart`，但**只在 Rust 侧用它**：
//! 前端调的是下面这两个命令，而不是插件的 JS API。这样不必给前端加依赖、
//! 也不必在 `capabilities` 里放开插件的权限 —— 少一层权限面，少一处可能配错。
//!
//! 两个刻意的设计：
//!
//! 1. **先改系统、再写配置**。反过来的话，注册表写失败会留下一份「配置说开着、
//!    实际没开」的谎报，而用户下次只能在任务管理器里困惑。
//! 2. 除了配置里的值，还提供一个 [`autostart_status`] 读**系统里的真实状态** ——
//!    用户可能用任务管理器/注册表把它关掉了。界面显示真实状态才有意义，
//!    否则会出现「勾是打着的、开机却没启动」。

use tauri::AppHandle;
use tauri::State;
use tauri_plugin_autostart::ManagerExt;

use crate::errors::{AppError, AppResult};
use crate::storage::config::{AppConfig, ConfigStore};

fn to_app_error(e: impl std::fmt::Display) -> AppError {
    AppError::internal(format!("设置开机自启失败：{e}"))
}

/// 开关开机自启，返回更新后的整份配置。
#[tauri::command]
pub async fn set_autostart(
    app: AppHandle,
    config: State<'_, ConfigStore>,
    enabled: bool,
) -> AppResult<AppConfig> {
    let launcher = app.autolaunch();
    if enabled {
        launcher.enable().map_err(to_app_error)?;
    } else {
        launcher.disable().map_err(to_app_error)?;
    }

    // 系统那边成功了才落盘：见上面第 1 条。
    config.update(|cfg| {
        cfg.autostart = enabled;
        Ok(cfg.clone())
    })
}

/// 读**系统里的真实状态**（可能与配置不一致：用户可能在任务管理器里关掉了）。
#[tauri::command]
pub async fn autostart_status(app: AppHandle) -> AppResult<bool> {
    app.autolaunch().is_enabled().map_err(to_app_error)
}

/// 自启时带的开关（见 [`is_minimized_launch`]）。
pub const MINIMIZED_FLAG: &str = "--minimized";

/// 命令行里有没有「最小化启动」这个开关。
///
/// 抽成接收迭代器的纯函数，是为了能在不碰真实 `std::env::args()` 的情况下单测：
/// 判断写错的话症状很隐蔽 —— 每次开机都弹一个置顶悬浮窗挡住视线，
/// 而开发时手动启动永远复现不了。
fn is_minimized_launch(args: impl Iterator<Item = String>) -> bool {
    args.into_iter().any(|a| a == MINIMIZED_FLAG)
}

/// 这一次是不是「开机自启带 `--minimized`」启动的？
///
/// 由前端在启动流程里问一次：是的话就不主动显示窗口。
/// 自启的真实意图是「它已经在后台待着，按 Alt+Space 就能出来」——
/// 不加这一条，每次开机都会有一个悬浮窗蹦到屏幕中间。
#[tauri::command]
pub fn started_minimized() -> bool {
    is_minimized_launch(std::env::args())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> impl Iterator<Item = String> {
        list.iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn the_minimized_flag_is_detected_anywhere_in_the_command_line() {
        assert!(is_minimized_launch(args(&["floatai.exe", "--minimized"])));
        // 参数顺序不保证；Windows 上还可能多出别的参数
        assert!(is_minimized_launch(args(&[
            "floatai.exe",
            "--flag",
            "--minimized",
            "extra",
        ])));
        // 只有开关、没有程序名（自启项里可能出现这种形式）
        assert!(is_minimized_launch(args(&["--minimized"])));
    }

    #[test]
    fn a_normal_launch_is_not_minimized() {
        assert!(!is_minimized_launch(args(&["floatai.exe"])));
        // 前缀相同但不是一个开关，不能被误判
        assert!(!is_minimized_launch(args(&[
            "floatai.exe",
            "--minimized-foo"
        ])));
        // 空参数列表也不能炸（测试环境/特殊启动方式下可能拿不到参数）
        assert!(!is_minimized_launch(args(&[])));
    }

    #[test]
    fn the_flag_constant_matches_what_the_plugin_was_configured_with() {
        // `lib.rs` 里注册插件时用的就是这几个字面量；两边必须一致，
        // 否则「自启时静默启动」会悄无声息地失效。
        assert_eq!(MINIMIZED_FLAG, "--minimized");
    }
}
