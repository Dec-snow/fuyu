//! 系统托盘图标与菜单。

use tauri::{
    image::Image,
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

use crate::window::{manager, shortcuts};

/// 编译期把托盘图标嵌进可执行文件，运行时不再依赖外部资源路径。
const TRAY_PNG: &[u8] = include_bytes!("../../icons/tray.png");

fn tray_icon() -> tauri::Result<Image<'static>> {
    Image::from_bytes(TRAY_PNG)
}

pub fn create(app: &tauri::App) -> tauri::Result<()> {
    let (shortcut, _) = shortcuts::configured(app);

    let toggle_item =
        MenuItemBuilder::with_id("toggle", format!("显示 / 隐藏（{shortcut}）")).build(app)?;
    let quit_item = MenuItemBuilder::with_id("quit", "退出浮语").build(app)?;
    let menu = MenuBuilder::new(app)
        .items(&[&toggle_item, &quit_item])
        .build()?;

    TrayIconBuilder::with_id("main-tray")
        .icon(tray_icon()?)
        .icon_as_template(false)
        .tooltip("浮语 · 桌面 AI 助手")
        .menu(&menu)
        // 左键直接开合窗口，右键才弹菜单；不这样设的话左键也会弹菜单。
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "toggle" => manager::toggle(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                manager::toggle(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}
