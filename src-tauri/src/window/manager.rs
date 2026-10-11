//! 悬浮窗的显示 / 隐藏 / 尺寸切换 / 位置记忆。

use std::sync::atomic::Ordering;
use std::time::Duration;

use tauri::{Emitter, LogicalSize, Manager, PhysicalPosition, WebviewWindow, Wry};

use crate::commands::window::HIDE_PENDING;
use crate::storage::config::{AppConfig, ConfigStore};

/// 悬浮模式下的窗口尺寸：一条输入栏 + 几行回答。
pub const FLOATING_WIDTH: f64 = 560.0;
pub const FLOATING_HEIGHT: f64 = 268.0;

/// 等前端播完退场动画的上限。
///
/// 超过这个时间就强制隐藏 —— 否则一旦渲染进程卡住或崩了，用户会得到一个
/// **关不掉的窗口**，只能去任务管理器杀进程。
const HIDE_GRACE_MS: u64 = 320;

pub fn main_window<M: Manager<Wry>>(app: &M) -> Option<WebviewWindow> {
    app.get_webview_window("main")
}

/// 显示窗口并抢焦点，必要时把它拉回屏幕内。
///
/// 动画完全由前端用 `transform` / `opacity` 驱动，后端**不**参与逐帧移动：
/// 每一帧都调 `set_position` 会走系统窗口管理器，抖动且昂贵。
pub fn show_and_focus(window: &WebviewWindow) {
    #[cfg(target_os = "windows")]
    force_rounded_corners(window);

    let target = resolve_position(window);
    let _ = window.set_position(target);
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();

    // 前端靠这个事件做两件事：
    // 1) 抑制「程序化 show 之后立刻到来的 blur」——否则会被失焦隐藏逻辑
    //    立刻又把窗口藏起来，表现为「按了快捷键窗口一闪而过」；
    // 2) 播放入场动画。
    let _ = window.emit("window-shown", true);
}

/// 快捷键 / 托盘左键的语义：可见且聚焦 → 隐藏；否则 → 显示。
pub fn toggle<M: Manager<Wry>>(app: &M) {
    let Some(window) = main_window(app) else {
        return;
    };
    let visible = window.is_visible().unwrap_or(false);
    let focused = window.is_focused().unwrap_or(false);

    if visible && focused {
        save_position(&window);
        hide_with_animation(&window);
    } else {
        show_and_focus(&window);
    }
}

/// 交给前端播退场动画，并挂一个兜底定时器。
pub fn hide_with_animation(window: &WebviewWindow) {
    let _ = window.emit("window-prepare-hide", true);
    arm_hide_fallback(window);
}

fn arm_hide_fallback(window: &WebviewWindow) {
    HIDE_PENDING.store(true, Ordering::SeqCst);
    let handle = window.app_handle().clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(HIDE_GRACE_MS));
        // 前端如果已经调过 `finish_hide`，这个标志会被换回 false。
        if !HIDE_PENDING.swap(false, Ordering::SeqCst) {
            return;
        }
        let inner = handle.clone();
        let _ = handle.run_on_main_thread(move || {
            if let Some(win) = inner.get_webview_window("main") {
                let _ = win.hide();
            }
        });
    });
}

/// 切换悬浮 / 完整模式，并按当前显示器重新居中。
pub fn apply_mode(window: &WebviewWindow, expanded: bool, cfg: &AppConfig) -> tauri::Result<()> {
    let (width, height) = if expanded {
        (cfg.expanded_width as f64, cfg.expanded_height as f64)
    } else {
        (FLOATING_WIDTH, FLOATING_HEIGHT)
    };
    window.set_size(LogicalSize::new(width, height))?;

    // 悬浮窗贴着屏幕上方 1/6 处更接近「呼之即来」的手感；完整模式居中。
    let y_ratio = if expanded { 0.5 } else { 1.0 / 6.0 };
    if let (Ok(Some(monitor)), Ok(size)) = (window.current_monitor(), window.outer_size()) {
        let m_pos = monitor.position();
        let m_size = monitor.size();
        let x = m_pos.x + (m_size.width as i32 - size.width as i32) / 2;
        let y = m_pos.y + ((m_size.height as i32 - size.height as i32) as f64 * y_ratio) as i32;
        let _ = window.set_position(PhysicalPosition::new(x, y.max(m_pos.y + 8)));
    }
    Ok(())
}

/// 记忆窗口位置。
pub fn save_position(window: &WebviewWindow) {
    let Ok(pos) = window.outer_position() else {
        return;
    };
    if let Some(store) = window.app_handle().try_state::<ConfigStore>() {
        let _ = store.update(|cfg| {
            if cfg.remember_window_position {
                cfg.window_x = Some(pos.x);
                cfg.window_y = Some(pos.y);
            }
            Ok(())
        });
    }
}

/// 决定这次显示时窗口落在哪里。
///
/// 注意是**算**出来的，而不是 `set_position` 之后再 `outer_position()` 读回来：
/// `set_position` 是异步的，紧接着读到的还是旧值 —— 参考项目就踩过这个坑，
/// 表现为「唤出动效飞回了窗口移动前的位置」。
fn resolve_position(window: &WebviewWindow) -> PhysicalPosition<i32> {
    let current = window
        .outer_position()
        .unwrap_or(PhysicalPosition::new(0, 0));

    let saved = window
        .app_handle()
        .try_state::<ConfigStore>()
        .and_then(|c| {
            c.read(|cfg| {
                if cfg.remember_window_position {
                    cfg.window_x.zip(cfg.window_y)
                } else {
                    None
                }
            })
        });

    if let Some((x, y)) = saved {
        if !mostly_off_screen(window, PhysicalPosition::new(x, y)) {
            return PhysicalPosition::new(x, y);
        }
    }

    // 当前位置几乎完全在屏幕外（例如拔掉了外接显示器）→ 重新居中。
    if mostly_off_screen(window, current) {
        if let (Ok(Some(monitor)), Ok(size)) = (window.current_monitor(), window.outer_size()) {
            let m_pos = monitor.position();
            let m_size = monitor.size();
            return PhysicalPosition::new(
                m_pos.x + (m_size.width as i32 - size.width as i32) / 2,
                m_pos.y + (m_size.height as i32 - size.height as i32) / 3,
            );
        }
    }

    current
}

fn mostly_off_screen(window: &WebviewWindow, pos: PhysicalPosition<i32>) -> bool {
    let (Ok(Some(monitor)), Ok(size)) = (window.current_monitor(), window.outer_size()) else {
        return false;
    };
    let m_pos = monitor.position();
    let m_size = monitor.size();
    let (mw, mh) = (m_size.width as i32, m_size.height as i32);
    let (ww, wh) = (size.width as i32, size.height as i32);

    pos.x + ww < m_pos.x + 48
        || pos.x > m_pos.x + mw - 48
        || pos.y + wh < m_pos.y + 48
        || pos.y > m_pos.y + mh - 48
}

/// 去掉窗口阴影之后，Windows 11 会连带丢掉圆角，这里显式要回来。
#[cfg(target_os = "windows")]
pub fn force_rounded_corners(window: &WebviewWindow) {
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };

    let Ok(hwnd) = window.hwnd() else { return };
    let preference = DWMWCP_ROUND;
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &preference as *const _ as *const _,
            std::mem::size_of_val(&preference) as u32,
        );
    }
}
