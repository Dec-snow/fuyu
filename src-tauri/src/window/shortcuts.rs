//! 全局快捷键注册：唤醒键（`Alt+Space`）+ 划词键（`Alt+Q`）。
//!
//! 按键的处理逻辑在 `lib.rs` 里的插件 builder（那里拿得到 `AppHandle`），
//! 这里只负责**按配置注册**、判断按的是哪个键、并把冲突上报给界面 ——
//! 注册失败绝不能静默吞掉，否则用户会以为「快捷键坏了」。

use tauri::{Emitter, Manager, Wry};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

use crate::storage::config::ConfigStore;

pub const DEFAULT_SHORTCUT: &str = "Alt+Space";
pub const DEFAULT_QUICK_ACTION_SHORTCUT: &str = "Alt+Q";

pub fn parse_shortcut(value: &str) -> Result<Shortcut, String> {
    value
        .trim()
        .parse::<Shortcut>()
        .map_err(|_| format!("快捷键格式不正确：{value}"))
}

/// 配置要求的两组键：`(唤醒键, 划词键)`。划词功能关闭时第二个为 `None`。
pub fn configured<M: Manager<Wry>>(app: &M) -> (String, Option<String>) {
    let Some(store) = app.try_state::<ConfigStore>() else {
        return (DEFAULT_SHORTCUT.to_string(), None);
    };

    let wake = store.read(|c| c.shortcut.clone());
    let wake = if wake.trim().is_empty() {
        DEFAULT_SHORTCUT.to_string()
    } else {
        wake
    };

    let quick = store.read(|c| {
        if !c.quick_action_enabled {
            return None;
        }
        Some(if c.quick_action_shortcut.trim().is_empty() {
            DEFAULT_QUICK_ACTION_SHORTCUT.to_string()
        } else {
            c.quick_action_shortcut.clone()
        })
    });

    (wake, quick)
}

/// 启动时按配置注册两组键。
pub fn register<M: Manager<Wry>>(app: &M) -> tauri::Result<()> {
    let (wake, quick) = configured(app);
    for error in register_all(app, &wake, quick.as_deref()) {
        eprintln!("{error}");
    }
    Ok(())
}

/// 设置页改**唤醒键**时调用（划词键保持不变）。失败会返回错误，前端据此拒绝。
pub fn register_value<M: Manager<Wry>>(app: &M, value: &str) -> Result<(), String> {
    let (_, quick) = configured(app);
    let errors = register_all(app, value, quick.as_deref());
    // 只把「和这次修改有关的错误」抛给前端，避免误伤划词键的冲突。
    match errors.into_iter().find(|e| e.contains(value)) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// 设置页改**划词键**时调用（唤醒键保持不变）。
///
/// Alt+Q 是个很抢手的键位（不少输入法、截图工具都占着），所以必须让用户能改。
pub fn register_quick_action_value<M: Manager<Wry>>(app: &M, value: &str) -> Result<(), String> {
    let (wake, _) = configured(app);
    let errors = register_all(app, &wake, Some(value));
    match errors.into_iter().find(|e| e.contains(value)) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// 按下的键是不是划词键？
pub fn is_quick_action<M: Manager<Wry>>(app: &M, pressed: &Shortcut) -> bool {
    let (_, quick) = configured(app);
    quick
        .and_then(|value| parse_shortcut(&value).ok())
        .is_some_and(|configured| configured == *pressed)
}

fn register_all<M: Manager<Wry>>(app: &M, wake: &str, quick: Option<&str>) -> Vec<String> {
    let global = app.global_shortcut();
    // 先全撤掉再注册：避免旧的还占着键位，导致新键「莫名其妙冲突」。
    let _ = global.unregister_all();

    let mut errors = Vec::new();
    for value in std::iter::once(wake).chain(quick) {
        match parse_shortcut(value) {
            Ok(shortcut) => {
                if let Err(e) = global.register(shortcut) {
                    errors.push(format!(
                        "快捷键 {value} 注册失败，可能已被其他程序占用：{e}"
                    ));
                }
            }
            Err(error) => errors.push(error),
        }
    }

    // 无论成败都通知前端，让设置面板能显示冲突提示。
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.emit("shortcut-error", errors.clone());
    }
    errors
}
