//! 划词取词：按 `Alt+Q` 抓取「其他程序中选中的文本」，唤起窗口并填入输入框。
//!
//! Windows 没有「读当前选区」的公开 API，所以业界通行做法（uTools、Snipaste…）
//! 是：**记住剪贴板 → 模拟 Ctrl+C → 轮询剪贴板 → 还原剪贴板**。
//!
//! 已知限制：模拟按键无法到达管理员权限（UAC 提升）的窗口 —— Windows 会挡住
//! 从普通完整性进程发往更高完整性进程的 `SendInput`。这时我们**明确报错**，
//! 而不是静默失败。

use std::time::Duration;

use tauri::Emitter;

/// 等目标程序把复制结果放进剪贴板的最长时间。
const POLL_TIMEOUT: Duration = Duration::from_millis(800);
const POLL_INTERVAL: Duration = Duration::from_millis(40);
/// 等热键的修饰键（Alt / Shift / Win）松开的最长时间。
///
/// 热键触发时 Alt 还物理按着，此时注入的 Ctrl+C 会变成 Alt+Ctrl+C，
/// 没有程序会把它当「复制」。等用户松手再发。
const MODIFIER_RELEASE_TIMEOUT: Duration = Duration::from_millis(600);

/// 热键入口：抓选区 → 唤起窗口 → 把文本交给前端。
///
/// 顺序很重要：模拟的 Ctrl+C 只会到达**当前拥有选区的那个窗口**，所以助手
/// 窗口必须等复制完成之后再显示 —— 否则按键会打进我们自己的输入框，取词
/// 永远失败。
pub fn trigger(app: &tauri::AppHandle) {
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = capture_selection();
        let inner = handle.clone();
        let _ = handle.run_on_main_thread(move || {
            let Some(window) = crate::window::manager::main_window(&inner) else {
                return;
            };
            crate::window::manager::show_and_focus(&window);
            let (event, payload) = match result {
                Ok(text) => ("quick-action", text),
                Err(error) => ("quick-action-error", error),
            };
            let _ = window.emit(event, payload);
        });
    });
}

/// 抓取当前选区。阻塞（剪贴板 + 轮询），必须在阻塞任务里调用。
pub fn capture_selection() -> Result<String, String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| format!("无法访问剪贴板：{e}"))?;
    let previous = clipboard.get_text().ok();

    wait_for_modifier_release(MODIFIER_RELEASE_TIMEOUT);
    send_copy()?;

    let deadline = std::time::Instant::now() + POLL_TIMEOUT;
    while std::time::Instant::now() < deadline {
        std::thread::sleep(POLL_INTERVAL);
        let Ok(text) = clipboard.get_text() else {
            continue;
        };
        if !is_fresh_copy(previous.as_deref(), &text) {
            continue;
        }
        restore(&mut clipboard, previous);
        return Ok(text.trim().to_string());
    }

    restore(&mut clipboard, previous);
    Err("未能获取选中文本：请先在目标程序里选中文字；管理员权限窗口无法通过热键取词".into())
}

/// 剪贴板里现在的内容，能不能证明「这次 Ctrl+C 确实复制到了东西」？
///
/// 两个必要条件：
/// 1. 非空（空白也算没复制到 —— 用户可能只选中了空格）；
/// 2. 与按热键前的内容**不同**。相同就说明复制压根没发生（没选中任何东西，
///    或者目标窗口被 UAC 挡住），此时等下去也是白等。
fn is_fresh_copy(previous: Option<&str>, current: &str) -> bool {
    !current.trim().is_empty() && previous != Some(current)
}

/// 把剪贴板还原成进来时的样子（尽力而为）。
fn restore(clipboard: &mut arboard::Clipboard, previous: Option<String>) {
    if let Some(text) = previous {
        let _ = clipboard.set_text(text);
    }
}

/// 等 Alt / Shift / Win 松开。Ctrl 刻意不等 —— 复制和弦本来就需要它。
#[cfg(windows)]
fn wait_for_modifier_release(timeout: Duration) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };

    fn held(vk: i32) -> bool {
        // 高位为 1 表示当前正按下。
        unsafe { (GetAsyncKeyState(vk) as u16) & 0x8000 != 0 }
    }

    let deadline = std::time::Instant::now() + timeout;
    loop {
        let busy = held(VK_MENU.0 as i32)
            || held(VK_SHIFT.0 as i32)
            || held(VK_LWIN.0 as i32)
            || held(VK_RWIN.0 as i32);
        if !busy || std::time::Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

#[cfg(not(windows))]
fn wait_for_modifier_release(_timeout: Duration) {}

#[cfg(windows)]
fn send_copy() -> Result<(), String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
        VIRTUAL_KEY, VK_C, VK_CONTROL,
    };

    fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if up {
                        KEYEVENTF_KEYUP
                    } else {
                        KEYBD_EVENT_FLAGS(0)
                    },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    let inputs = [
        key(VK_CONTROL, false),
        key(VK_C, false),
        key(VK_C, true),
        key(VK_CONTROL, true),
    ];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent == 0 {
        return Err("模拟复制按键失败（目标窗口可能是管理员权限）".into());
    }
    Ok(())
}

#[cfg(not(windows))]
fn send_copy() -> Result<(), String> {
    Err("当前平台暂不支持划词取词".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_action_timeouts_are_sane() {
        // 抓取路径需要真实桌面会话，这里只校验不可交互的常量。
        assert!(POLL_TIMEOUT.as_millis() >= 500);
        assert!(MODIFIER_RELEASE_TIMEOUT.as_millis() >= 200);
    }

    #[test]
    fn fresh_copy_requires_new_non_empty_content() {
        // 有内容且与之前不同 → 复制成功。
        assert!(is_fresh_copy(Some("旧内容"), "新选中的文字"));
        // 之前剪贴板是空的 / 读不到 → 任何非空内容都算新。
        assert!(is_fresh_copy(None, "选中文字"));

        // 内容没变 → 复制没发生。
        assert!(!is_fresh_copy(Some("一样"), "一样"));
        // 空白不算复制到东西。
        assert!(!is_fresh_copy(None, ""));
        assert!(!is_fresh_copy(None, "   \n\t "));
        // 从「有内容」变成「空白」也不是有效取词。
        assert!(!is_fresh_copy(Some("旧内容"), "  "));
    }
}
