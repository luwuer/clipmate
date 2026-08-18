//! ClipMate — macOS 剪贴板历史工具（CleanClip 风格）
//! Tauri 2 + Rust 后端 + vanilla JS 前端。
//!
//! R7 模块拆分（纯移动，零行为变化）：
//! - model.rs     数据模型 + 去重/上限纯逻辑 + png 编解码 + 单测
//! - clipboard.rs NSPasteboard changeCount 轮询 + 捕获
//! - paste.rs     CGEvent Cmd+V 模拟 + AX 辅助（只读）+ frontmost/activate
//! - panel.rs     non-activating NSPanel 转换/显示/定位 + 点击外部关闭
//! - commands.rs  tauri commands（get_history/select/copy/delete/pin/…）
//! - storage.rs   JSONL 持久化（R1）/ menubar.rs 菜单栏（R3）——此前已独立
//! - main.rs      本文件：builder + setup + 快捷键配置

mod clipboard;
mod commands;
mod menubar;
mod model;
mod panel;
mod paste;
mod storage;

// re-export：保持 storage.rs / menubar.rs 的 crate:: 根路径引用不变（零改动）
pub use model::{AppState, ClipboardItem, ItemKind};
pub use panel::toggle_panel_pub;
pub use tauri::AppHandle;

use std::sync::atomic::Ordering;
use std::time::Duration;

use tauri::Manager;
use tauri_plugin_global_shortcut::ShortcutState;

const DEFAULT_HOTKEY: &str = "F2"; // 配置缺失/非法时回退
pub(crate) const DEFAULT_THEME: &str = "dark"; // theme 缺失/非法时回退

/// 读取 settings.json 的 theme 字段（"dark"|"light"）；缺失/非法返回 DEFAULT_THEME
pub(crate) fn read_theme_from_settings(data_dir: &std::path::Path) -> String {
    use serde_json::Value;
    let path = data_dir.join("settings.json");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return DEFAULT_THEME.to_string();
    };
    let Ok(v) = serde_json::from_str::<Value>(&content) else {
        return DEFAULT_THEME.to_string();
    };
    match v.get("theme").and_then(|x| x.as_str()) {
        Some("light") => "light".to_string(),
        _ => DEFAULT_THEME.to_string(),
    }
}

/// 写入 theme 字段到 settings.json，保留其他字段（hotkey 等）
pub(crate) fn write_theme_to_settings(data_dir: &std::path::Path, theme: &str) {
    use serde_json::Value;
    let path = data_dir.join("settings.json");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut v: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(obj) = v.as_object_mut() {
        obj.insert("theme".to_string(), Value::String(theme.to_string()));
    }
    let _ = std::fs::write(&path, v.to_string());
}

// ---------- R4: 快捷键配置（settings.json，缺失/解析失败回退 F2） ----------

/// 读取 app_data_dir/settings.json 的 hotkey 字段；缺失字段或解析失败返回 DEFAULT_HOTKEY
/// 首次启动时若 settings.json 不存在，自动写一份默认（=DEFAULT_HOTKEY），方便用户编辑
fn read_hotkey_from_settings(data_dir: &std::path::Path) -> String {
    use serde_json::Value;
    let path = data_dir.join("settings.json");
    // 缺失 → 写默认
    let Ok(content) = std::fs::read_to_string(&path) else {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, serde_json::json!({ "hotkey": DEFAULT_HOTKEY }).to_string());
        return DEFAULT_HOTKEY.to_string();
    };
    let Ok(v) = serde_json::from_str::<Value>(&content) else {
        eprintln!("[clipmate] settings.json parse failed, fallback to {DEFAULT_HOTKEY}");
        return DEFAULT_HOTKEY.to_string();
    };
    match v.get("hotkey").and_then(|x| x.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            eprintln!("[clipmate] settings.json missing/invalid 'hotkey', fallback to {DEFAULT_HOTKEY}");
            DEFAULT_HOTKEY.to_string()
        }
    }
}

fn main() {
    eprintln!("[clipmate] starting…");
    let app = tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        panel::toggle_panel(app);
                    }
                })
                .build(),
        )
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            commands::get_history,
            commands::select_item,
            commands::batch_select,
            commands::batch_delete,
            commands::copy_item,
            commands::delete_item,
            commands::clear_history,
            commands::toggle_pin,
            commands::hide_panel,
            commands::drag_begin,
            commands::drag_move,
            commands::drag_end,
            commands::is_ax_trusted,
            commands::open_accessibility_settings,
            commands::copy_tccutil_command,
            commands::get_theme,
            commands::set_theme
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // ---- 历史持久化：加载既有条目 + 启动防抖落盘线程 ----
            let data_dir = app.path().app_data_dir().expect("app_data_dir unavailable");
            let storage = storage::Storage::new(data_dir.clone());
            let mut loaded = storage.load();
            let n_loaded = loaded.len();
            if n_loaded > 0 {
                let state = app.state::<AppState>();
                let max_id = loaded.iter().map(|it| it.id).max().unwrap_or(0);
                state.next_id.store(max_id + 1, Ordering::SeqCst); // 防 id 冲突
                // R6: 加载路径同样执行上限策略——落盘上限(500)大于内存上限(300)，
                // 不补这一步则重启后 unpinned 条目可超限，直到下一条新剪贴内容才收敛
                model::enforce_limit(&mut loaded);
                *state.items.lock().unwrap() = loaded;
            }
            eprintln!("[clipmate] loaded {n_loaded} persisted items");
            app.manage(storage.clone());
            storage.start_flusher(app.handle().clone());

            // register the global hotkey (from settings.json, fallback F2)
            use tauri_plugin_global_shortcut::GlobalShortcutExt;
            // R4: 读 settings.json 的 hotkey，缺失/非法回退 DEFAULT_HOTKEY
            let cfg_hotkey = read_hotkey_from_settings(&data_dir);
            match app.global_shortcut().register(cfg_hotkey.as_str()) {
                Ok(_) => eprintln!("[clipmate] hotkey {cfg_hotkey} registered"),
                Err(e) => {
                    eprintln!("[clipmate] hotkey '{cfg_hotkey}' register failed: {e}, fallback to {DEFAULT_HOTKEY}");
                    let _ = app.global_shortcut().register(DEFAULT_HOTKEY);
                }
            }

            // 把窗口转成「不激活应用」的 NSPanel —— 全程不抢目标应用焦点
            if let Some(win) = app.get_webview_window("main") {
                panel::convert_to_panel(&win);

                // 点击面板外时自动隐藏（面板 resign key / 应用失焦都覆盖）
                let win_clone = win.clone();
                win.on_window_event(move |event| {
                    if let tauri::WindowEvent::Focused(false) = event {
                        let state = win_clone.state::<AppState>();
                        // 拖拽期间失焦不隐藏（用户正在按住拖动面板）
                        if state.dragging.load(Ordering::SeqCst) {
                            return;
                        }
                        let recently_shown = state
                            .shown_at
                            .lock()
                            .unwrap()
                            .map(|t| t.elapsed() < Duration::from_millis(150))
                            .unwrap_or(false);
                        if !recently_shown {
                            panel::panel_hide_ns(&win_clone);
                        }
                    }
                });
            }

            clipboard::start_poller(app.handle().clone());
            panel::install_mouse_monitor(app.handle());
            // R3: 安装菜单栏图标 + 退出入口（AppHandle 按值传入，Box 泄漏存全局）
            menubar::install(app.handle().clone());

            // 启动时若无辅助功能权限，触发一次系统授权弹窗（粘贴功能必需：
            // CGEventPost 模拟 Cmd+V 在 macOS 10.14+ 需要该权限，否则事件被静默丢弃）
            let _ = std::thread::spawn(|| {
                std::thread::sleep(Duration::from_millis(500));
                if !paste::ax_trusted(false) {
                    paste::ax_trusted(true);
                }
            });

            eprintln!("[clipmate] setup complete");

            if std::env::args().any(|a| a == "--show") {
                panel::show_panel(&app.handle().clone());
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building ClipMate");

    // R5: 退出前同步落盘一次，消除 2s 防抖窗口内的变更丢失
    // （menubar「退出」app.exit(0) 与正常退出路径都会经过 RunEvent::Exit）
    app.run(|app, event| {
        if let tauri::RunEvent::Exit = event {
            if let Some(st) = app.try_state::<storage::Storage>() {
                st.flush_now(app);
            }
        }
    });
}
