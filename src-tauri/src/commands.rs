//! tauri commands（R7 从 main.rs 拆出，零行为变化）

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use base64::Engine;
use tauri::{AppHandle, Manager, State};

use crate::clipboard::pasteboard_change_count;
use crate::model::{AppState, ClipboardItem, ItemDto, ItemKind, decode_png};
use crate::panel::{panel_drag_anchor, panel_drag_apply, panel_hide_ns};
use crate::paste::{activate_app, ax_trusted, ensure_ax, frontmost_pid, paste_cmd_v};
use crate::storage;

const PASTE_DELAY_MS: u64 = 150;

fn to_dto(it: &ClipboardItem) -> ItemDto {
    match &it.kind {
        ItemKind::Text(t) => {
            let preview: String = t.chars().take(300).collect();
            ItemDto {
                id: it.id,
                item_type: "text",
                text: Some(preview),
                image: None,
                width: None,
                height: None,
                time: it.created_at,
                pinned: it.pinned,
            }
        }
        ItemKind::Image { png, width, height } => ItemDto {
            id: it.id,
            item_type: "image",
            text: None,
            image: Some(format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(png)
            )),
            width: Some(*width),
            height: Some(*height),
            time: it.created_at,
            pinned: it.pinned,
        },
    }
}

#[tauri::command]
pub(crate) fn get_history(state: State<'_, AppState>, query: String) -> Vec<ItemDto> {
    let items = state.items.lock().unwrap();
    let q = query.trim().to_lowercase();
    let mut filtered: Vec<&ClipboardItem> = items
        .iter()
        .filter(|it| match (&it.kind, q.is_empty()) {
            (_, true) => true,
            (ItemKind::Text(t), false) => t.to_lowercase().contains(&q),
            (ItemKind::Image { .. }, false) => {
                // 既要"图"能匹配图片，又要"截图""image"也能匹配
                ["图片", "截图", "image", "img", "png"]
                    .iter()
                    .any(|k| q.contains(k) || k.contains(&q))
            }
        })
        .collect();
    // R2: 置顶项优先；同组按时间倒序
    filtered.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.created_at.cmp(&a.created_at)));
    filtered.into_iter().take(200).map(to_dto).collect()
}

fn take_item(state: &State<'_, AppState>, id: u64) -> Result<ClipboardItem, String> {
    let mut items = state.items.lock().unwrap();
    let pos = items
        .iter()
        .position(|it| it.id == id)
        .ok_or_else(|| "item not found".to_string())?;
    let it = items.remove(pos);
    items.insert(0, it.clone());
    Ok(it)
}

#[tauri::command]
pub(crate) fn select_item(app: AppHandle, state: State<'_, AppState>, id: u64) -> Result<(), String> {
    // CGEventPost 模拟 Cmd+V 需要辅助功能权限；未授权时触发系统弹窗引导并中止本次粘贴
    if !ensure_ax() {
        return Err("NEED_AX_PERMISSION".into());
    }

    let item = take_item(&state, id)?;
    app.state::<storage::Storage>().request_save(); // 重排后持久化

    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    match &item.kind {
        ItemKind::Text(t) => cb.set_text(t.clone()).map_err(|e| e.to_string())?,
        ItemKind::Image { png, .. } => {
            let img = decode_png(png).ok_or_else(|| "image decode failed".to_string())?;
            cb.set_image(img).map_err(|e| e.to_string())?;
        }
    }
    // advance the poller baseline so our own write is not re-recorded
    state
        .last_change_count
        .fetch_max(pasteboard_change_count(), Ordering::SeqCst);

    // 隐藏面板；wait/paste 放到后台线程，不冻结 AppKit 主事件循环
    if let Some(win) = app.get_webview_window("main") {
        panel_hide_ns(&win);
    }
    let target_pid = state.prev_front_pid.load(Ordering::SeqCst);
    std::thread::spawn(move || {
        if target_pid > 0 {
            activate_app(target_pid);
            let deadline = Instant::now() + Duration::from_millis(400);
            while Instant::now() < deadline {
                if frontmost_pid() == target_pid {
                    break;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            std::thread::sleep(Duration::from_millis(60));
        } else {
            std::thread::sleep(Duration::from_millis(PASTE_DELAY_MS));
        }
        paste_cmd_v();
    });
    Ok(())
}

#[tauri::command]
pub(crate) fn copy_item(app: AppHandle, state: State<'_, AppState>, id: u64) -> Result<(), String> {
    let item = take_item(&state, id)?;
    app.state::<storage::Storage>().request_save(); // 重排后持久化
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    match &item.kind {
        ItemKind::Text(t) => cb.set_text(t.clone()).map_err(|e| e.to_string())?,
        ItemKind::Image { png, .. } => {
            let img = decode_png(png).ok_or_else(|| "image decode failed".to_string())?;
            cb.set_image(img).map_err(|e| e.to_string())?;
        }
    }
    // advance the poller baseline so our own write is not re-recorded
    // （fetch_max 与 select_item/copy_tccutil_command 口径一致，防 baseline 回退）
    state
        .last_change_count
        .fetch_max(pasteboard_change_count(), Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
pub(crate) fn delete_item(app: AppHandle, state: State<'_, AppState>, id: u64) -> Result<(), String> {
    let mut items = state.items.lock().unwrap();
    items.retain(|it| it.id != id);
    drop(items);
    app.state::<storage::Storage>().request_save();
    Ok(())
}

#[tauri::command]
pub(crate) fn clear_history(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    state.items.lock().unwrap().clear();
    app.state::<storage::Storage>().request_save(); // 清空后落盘 → jsonl 重写为空
    Ok(())
}

#[tauri::command]
pub(crate) fn toggle_pin(app: AppHandle, state: State<'_, AppState>, id: u64) -> Result<bool, String> {
    let mut items = state.items.lock().unwrap();
    let it = items.iter_mut().find(|it| it.id == id).ok_or_else(|| "item not found".to_string())?;
    it.pinned = !it.pinned;
    let new_state = it.pinned;
    drop(items);
    app.state::<storage::Storage>().request_save();
    Ok(new_state)
}

#[tauri::command]
pub(crate) fn hide_panel(app: AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        panel_hide_ns(&win);
    }
}

// ---------- 标题栏拖拽（前端 mousedown/mousemove/mouseup 手工追踪） ----------

/// 拖拽开始：记录鼠标与窗口 origin 的锚点（Web 屏幕坐标 → AppKit 坐标在 panel.rs 转换）
#[tauri::command]
pub(crate) fn drag_begin(app: AppHandle, state: State<'_, AppState>, x: f64, y: f64) {
    if let Some(win) = app.get_webview_window("main") {
        if let Some(anchor) = panel_drag_anchor(&win, x, y) {
            *state.drag_state.lock().unwrap() = Some(anchor);
            state.dragging.store(true, Ordering::SeqCst);
        }
    }
}

/// 拖拽移动：按锚点 + 当前鼠标位置 setFrameOrigin（前端已 rAF 节流）
#[tauri::command]
pub(crate) fn drag_move(app: AppHandle, state: State<'_, AppState>, x: f64, y: f64) {
    let anchor = *state.drag_state.lock().unwrap();
    if let (Some(win), Some(anchor)) = (app.get_webview_window("main"), anchor) {
        panel_drag_apply(&win, x, y, anchor);
    }
}

/// 拖拽结束：清除拖拽标记（恢复 blur-hide / 点击外部隐藏）
#[tauri::command]
pub(crate) fn drag_end(state: State<'_, AppState>) {
    state.dragging.store(false, Ordering::SeqCst);
    *state.drag_state.lock().unwrap() = None;
}

#[tauri::command]
pub(crate) fn is_ax_trusted() -> bool {
    ax_trusted(false)
}

// ---------- 主题（settings.json 的 theme 字段，"dark"|"light"） ----------

#[tauri::command]
pub(crate) fn get_theme(app: AppHandle) -> String {
    let Ok(dir) = app.path().app_data_dir() else {
        return crate::DEFAULT_THEME.to_string();
    };
    crate::read_theme_from_settings(&dir)
}

#[tauri::command]
pub(crate) fn set_theme(app: AppHandle, theme: String) -> Result<(), String> {
    let theme = if theme == "light" { "light" } else { "dark" };
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    crate::write_theme_to_settings(&dir, theme);
    use tauri::Emitter;
    let _ = app.emit("theme-changed", theme);
    Ok(())
}

#[tauri::command]
pub(crate) fn open_accessibility_settings() {
    // 先触发一次系统请求弹窗（即使之前拒绝过也再试一次，让 ClipMate 出现在列表里）
    let _ = std::process::Command::new("osascript")
        .arg("-e")
        .arg(r#"tell application "System Events" to keystroke "" "#)
        .spawn();
    ax_trusted(true);
    // 同时打开辅助功能设置页（双 URL 兼容 macOS 13/15）
    crate::paste::open_accessibility_settings_page();
}

#[tauri::command]
pub(crate) fn copy_tccutil_command(state: State<'_, AppState>) -> Result<(), String> {
    let cmd = "tccutil reset Accessibility com.mdy.clipmate";
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_text(cmd.to_string()).map_err(|e| e.to_string())?;
    // 推进 baseline 防止 poller 把这条命令文本作为历史记录
    state
        .last_change_count
        .fetch_max(pasteboard_change_count(), Ordering::SeqCst);
    Ok(())
}
