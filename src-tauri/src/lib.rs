//! 应用装配入口。
//!
//! 启动顺序（顺序有讲究）：
//! 1. 注册单实例插件 —— 第二次启动只唤起已有窗口，不新开进程；
//! 2. 注册全局快捷键插件 —— 按键回调在 `lib.rs` 里，因为它拿得到 `AppHandle`；
//! 3. `manage(AppState)` —— 先建 HTTP 客户端（此时还不知道代理配置）；
//! 4. `invoke_handler` —— 声明所有命令；
//! 5. `setup` —— 读配置、开数据库、按配置重建 HTTP 客户端、建托盘、注册快捷键。

mod agent;
mod api;
mod commands;
mod diagnostics;
mod documents;
mod errors;
mod kb;
mod memory;
mod models;
mod state;
mod storage;
mod tools;
mod window;

use tauri::Manager;

pub fn run() {
    tauri::Builder::default()
        // 第二次启动 → 唤起已有窗口，而不是起第二个进程。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                window::manager::show_and_focus(&window);
            }
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    // 只响应按下，否则会「按一次开、松手又关」。
                    if event.state != tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        return;
                    }
                    if window::shortcuts::is_quick_action(app, shortcut) {
                        window::quick_action::trigger(app);
                    } else {
                        window::manager::toggle(app);
                    }
                })
                .build(),
        )
        // 开机自启插件。**只在 Rust 侧用它**（见 `commands/autostart.rs`）：
        // 前端调我们自己的命令，所以不必给前端加依赖、也不必在 capabilities
        // 里放开插件权限 —— 少一层权限面。
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            // 自启时带上 `--minimized`：开机时直接弹出一个置顶悬浮窗很打扰，
            // 用户要的是「它已经在后台待着，按 Alt+Space 就能出来」。
            Some(vec!["--minimized"]),
        ))
        .manage(state::AppState::new())
        .invoke_handler(tauri::generate_handler![
            // ---- 对话
            commands::chat::chat_send,
            commands::chat::chat_cancel,
            commands::chat::chat_approve_tool,
            commands::chat::test_provider,
            // ---- 剪贴板
            commands::clipboard::copy_to_clipboard,
            // ---- 备份
            commands::backup::export_history,
            commands::backup::import_history,
            // ---- 诊断与数据管理
            commands::diagnostics::diagnostics,
            commands::diagnostics::clear_logs,
            commands::diagnostics::reset_settings,
            commands::diagnostics::delete_all_api_keys,
            // ---- 开机自启
            commands::autostart::set_autostart,
            commands::autostart::autostart_status,
            commands::autostart::started_minimized,
            // ---- 用量与费用
            commands::usage::usage_report,
            // ---- 长期记忆
            commands::memory::memory_list,
            commands::memory::memory_stats,
            commands::memory::memory_add,
            commands::memory::memory_delete,
            commands::memory::memory_relevant,
            // ---- 本地知识库
            commands::kb::kb_list,
            commands::kb::kb_stats,
            commands::kb::kb_add_text,
            commands::kb::kb_add_document,
            commands::kb::kb_delete,
            commands::kb::kb_search,
            // ---- 文档解析
            commands::documents::extract_document,
            // ---- 设置
            commands::settings::app_info,
            commands::settings::get_config,
            commands::settings::update_settings,
            commands::settings::apply_proxy,
            // ---- 服务商与模型
            commands::providers::get_providers_state,
            commands::providers::upsert_provider,
            commands::providers::delete_provider,
            commands::providers::add_models,
            commands::providers::remove_model,
            commands::providers::set_default_model,
            commands::providers::set_model_favorite,
            commands::providers::set_api_key,
            commands::providers::fetch_models,
            // ---- 历史
            commands::history::list_conversations,
            commands::history::get_conversation,
            commands::history::create_conversation,
            commands::history::new_conversation_id,
            commands::history::rename_conversation,
            commands::history::set_conversation_pinned,
            commands::history::set_conversation_role,
            commands::history::delete_conversation,
            commands::history::clear_all_history,
            commands::history::save_user_message,
            commands::history::update_message,
            commands::history::delete_message,
            commands::history::messages_before,
            commands::history::search_messages,
            commands::history::token_totals,
            // ---- 窗口
            commands::window::show_window,
            commands::window::hide_window,
            commands::window::finish_hide,
            commands::window::toggle_window,
            commands::window::set_window_mode,
            commands::window::save_window_position,
            commands::window::get_shortcut,
            commands::window::set_shortcut,
            commands::window::set_quick_action_shortcut,
            commands::window::reapply_shortcuts,
        ])
        .setup(|app| {
            // ---- 配置目录（providers.json）
            let config_dir = app
                .path()
                .app_config_dir()
                .map_err(|e| format!("无法解析配置目录：{e}"))?;
            std::fs::create_dir_all(&config_dir).map_err(|e| format!("无法创建配置目录：{e}"))?;

            let config_store =
                storage::config::ConfigStore::load(config_dir.join("providers.json"));

            // 代理配置读出来之后重建一次 HTTP 客户端。
            //
            // 以前只在 `proxy_url` 非空时才重建，所以 `use_system_proxy`
            // 填了 `false` 也不会关掉环境变量代理 —— 现在两个值都参与，
            // 无条件重建一次，让配置和实际客户端一致。
            let (proxy, use_system_proxy) =
                config_store.read(|cfg| (cfg.proxy_url.clone(), cfg.use_system_proxy));
            app.state::<state::AppState>()
                .rebuild_http_client(proxy.as_deref(), use_system_proxy);
            app.manage(config_store);

            // ---- 数据目录（SQLite），迁移在 `open` 里完成
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("无法解析数据目录：{e}"))?;
            let db = storage::database::Database::open(&data_dir.join("floatai.db"))
                .map_err(|e| format!("数据库初始化失败：{e}"))?;
            app.manage(db);

            // ---- 托盘与全局快捷键
            window::tray::create(app)?;
            window::shortcuts::register(app)?;

            // 去阴影 + 显式要回圆角：入场动效是一个「大水花」从托盘位置
            // 飞到窗口位置，如果留着系统阴影，会先把窗口的真实尺寸暴露出来，
            // 看起来像闪了一下。
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_shadow(false);
                #[cfg(target_os = "windows")]
                window::manager::force_rounded_corners(&window);
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("构建 FloatAI 失败")
        .run(|_app_handle, _event| {});
}
