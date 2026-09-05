//! ClipMate — 跨平台剪贴板历史工具（macOS / Windows，CleanClip 风格）
//! Tauri 2 + Rust 后端 + Vue 3 前端。
//!
//! R7 模块拆分（纯移动，零行为变化）；W1 Windows 移植：平台代码为 macos_impl / windows_impl 成对实现
//! - model.rs     数据模型 + 去重/上限纯逻辑 + png 编解码 + 单测
//! - clipboard.rs 剪贴板变化检测（NSPasteboard changeCount / GetClipboardSequenceNumber）
//! - paste.rs     粘贴模拟 + 焦点管理（CGEvent Cmd+V / SendInput Ctrl+V）
//! - panel.rs     面板窗口与定位（NSPanel / WS_EX_TOOLWINDOW）+ 点击外部关闭
//! - commands.rs  tauri commands（get_history/select/copy/delete/pin/…）
//! - storage.rs   JSONL 持久化（R1）/ menubar.rs 菜单栏与托盘（R3）——此前已独立
//! - main.rs      本文件：builder + setup + 快捷键配置

mod autostart;
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
/// 面板位置模式："fixed"（光标所在屏幕顶部居中）| "cursor"（贴光标，旧行为）
pub(crate) const DEFAULT_PANEL_POSITION: &str = "fixed";

/// 读取 settings.json 的 panel_position 字段（"fixed"|"cursor"）；缺失/非法回退 DEFAULT_PANEL_POSITION
pub(crate) fn read_panel_position_from_settings(data_dir: &std::path::Path) -> String {
    use serde_json::Value;
    let path = data_dir.join("settings.json");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return DEFAULT_PANEL_POSITION.to_string();
    };
    let Ok(v) = serde_json::from_str::<Value>(&content) else {
        return DEFAULT_PANEL_POSITION.to_string();
    };
    match v.get("panel_position").and_then(|x| x.as_str()) {
        Some("cursor") => "cursor".to_string(),
        _ => DEFAULT_PANEL_POSITION.to_string(),
    }
}

/// 写入 panel_position 到 settings.json，保留其他字段
pub(crate) fn write_panel_position_to_settings(data_dir: &std::path::Path, mode: &str) {
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
        obj.insert(
            "panel_position".to_string(),
            Value::String(mode.to_string()),
        );
    }
    let _ = std::fs::write(&path, v.to_string());
}

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

/// 读取 settings.json 的 background_image 字段（自定义背景图绝对路径）；缺失/非法返回 None
pub(crate) fn read_background_from_settings(data_dir: &std::path::Path) -> Option<String> {
    use serde_json::Value;
    let path = data_dir.join("settings.json");
    let content = std::fs::read_to_string(&path).ok()?;
    let v: Value = serde_json::from_str(&content).ok()?;
    v.get("background_image")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// 写入/清除 background_image 字段（None = 移除字段），保留其他字段
pub(crate) fn write_background_to_settings(data_dir: &std::path::Path, bg: Option<&str>) {
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
        match bg {
            Some(s) => {
                obj.insert(
                    "background_image".to_string(),
                    Value::String(s.to_string()),
                );
            }
            None => {
                obj.remove("background_image");
            }
        }
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
            commands::get_item_detail,
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
            commands::set_theme,
            commands::get_background_image,
            commands::bg_shot_report
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

            // R20: 开机自启 plist 路径漂移修正（用户移动/重装 .app 后自动更新）
            autostart::sync_path_if_stale();

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

            // ---- 自测模式：程序化复现「打开→滚动→关闭→再打开」白屏 bug ----
            // 用途：自动化复现/验证 WKWebView 在 orderOut→makeKeyAndOrderFront 后
            // 左侧列表不绘制的 bug，配合外部 screencapture 截图比对。
            // 时间线（秒）：1.0 show#1 → 2.5 滚到底 → 3.5 hide → 5.0 show#2 → 一直显示
            if std::env::args().any(|a| a == "--selftest-repro") {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    let show = |h: &tauri::AppHandle| {
                        let h2 = h.clone();
                        let _ = h.clone().run_on_main_thread(move || panel::show_panel(&h2));
                    };
                    let hide = |h: &tauri::AppHandle| {
                        let h2 = h.clone();
                        let _ = h.clone().run_on_main_thread(move || {
                            if let Some(w) = h2.get_webview_window("main") {
                                panel::panel_hide_ns(&w);
                            }
                        });
                    };
                    let eval = |h: &tauri::AppHandle, js: &str| {
                        if let Some(w) = h.get_webview_window("main") {
                            let _ = w.eval(js);
                        }
                    };
                    std::thread::sleep(Duration::from_millis(1000));
                    eprintln!("[selftest] t=1.0 show #1");
                    show(&handle);
                    std::thread::sleep(Duration::from_millis(1500));
                    eprintln!("[selftest] t=2.5 scroll to bottom");
                    eval(&handle, "(()=>{const l=document.querySelector('.list');if(l){l.scrollTop=l.scrollHeight;}})()");
                    std::thread::sleep(Duration::from_millis(1000));
                    eprintln!("[selftest] t=3.5 hide");
                    hide(&handle);
                    std::thread::sleep(Duration::from_millis(1500));
                    eprintln!("[selftest] t=5.0 show #2  ← 白屏判定窗口（6s 后截屏）");
                    show(&handle);
                    std::thread::sleep(Duration::from_millis(8000));
                    eprintln!("[selftest] t=13.0 hide + 结束");
                    hide(&handle);
                });
            }

            // ---- 背景图截图模式：单纯 show panel + 等待渲染 + 不自动关闭 ----
            // 配合外部 screencapture 截图验证预设/自定义背景渲染效果；
            // 不滚动、不关闭，避免 WKWebView 合成层缓存的副作用。
            if std::env::args().any(|a| a == "--bg-shot") {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(1500));
                    eprintln!("[bg-shot] show panel");
                    let h2 = handle.clone();
                    let _ = handle.run_on_main_thread(move || panel::show_panel(&h2));
                    // 3s 后 eval 抓 DOM 状态并通过 invoke 回传
                    std::thread::sleep(Duration::from_millis(3000));
                    if let Some(w) = handle.get_webview_window("main") {
                        let js = r#"window.__TAURI__.core.invoke('bg_shot_report', { report: (()=>{
                            const p=document.querySelector('.panel');
                            const media=document.querySelector('.bg-media');
                            const logs=[];
                            logs.push('panel.className = ' + p.className);
                            logs.push('panel inline style = ' + (p.getAttribute('style')||'null'));
                            if (media) {
                                logs.push('bg-media tag=' + media.tagName + ' src=' + (media.src||'').slice(0,50) + '…');
                                if (media.tagName === 'VIDEO') {
                                    logs.push('video readyState=' + media.readyState + ' paused=' + media.paused + ' currentTime=' + media.currentTime + ' size=' + media.videoWidth + 'x' + media.videoHeight);
                                    // 2.5s 后回传播放心跳（currentTime 前进 = 真在播）
                                    setTimeout(()=> window.__TAURI__.core.invoke('bg_shot_report', { report: 'VIDEO_TICK currentTime='+media.currentTime.toFixed(2)+' readyState='+media.readyState+' paused='+media.paused }), 2500);
                                } else {
                                    logs.push('img natural=' + media.naturalWidth + 'x' + media.naturalHeight + ' complete=' + media.complete);
                                }
                            } else {
                                logs.push('bg-media: NOT FOUND');
                            }
                            const afterCs = getComputedStyle(p, '::after');
                            logs.push('::after background = ' + afterCs.background.slice(0,100));
                            return logs.join('\n');
                        })() })"#;
                        match w.eval(js) {
                            Ok(_) => eprintln!("[bg-shot] eval dispatched"),
                            Err(e) => eprintln!("[bg-shot] eval failed: {e}"),
                        }
                    }
                    // 持续显示
                });
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
