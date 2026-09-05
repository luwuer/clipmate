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

/// 详情 DTO：与列表 ItemDto 的差异是 text 不截断，且附带统计信息（字符/行/字节）
#[derive(serde::Serialize)]
pub(crate) struct DetailDto {
    id: u64,
    #[serde(rename = "type")]
    item_type: &'static str,
    pinned: bool,
    time: u64,
    text: Option<String>,  // 完整文本（不截断）
    chars: Option<usize>,  // 字符数（按 Unicode 标量计）
    lines: Option<usize>,  // 行数
    bytes: usize,          // 文本=UTF-8 字节数；图片=PNG 体积
    image: Option<String>, // data url（完整图）
    width: Option<u32>,
    height: Option<u32>,
}

fn to_detail(it: &ClipboardItem) -> DetailDto {
    match &it.kind {
        ItemKind::Text(t) => DetailDto {
            id: it.id,
            item_type: "text",
            pinned: it.pinned,
            time: it.created_at,
            chars: Some(t.chars().count()),
            lines: Some(t.lines().count()),
            bytes: t.len(),
            text: Some(t.clone()),
            image: None,
            width: None,
            height: None,
        },
        ItemKind::Image { png, width, height } => DetailDto {
            id: it.id,
            item_type: "image",
            pinned: it.pinned,
            time: it.created_at,
            chars: None,
            lines: None,
            bytes: png.len(),
            text: None,
            image: Some(format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(png)
            )),
            width: Some(*width),
            height: Some(*height),
        },
    }
}

/// 单条详情：右侧详情面板数据源（列表项仅含 300 字符预览）
#[tauri::command]
pub(crate) fn get_item_detail(state: State<'_, AppState>, id: u64) -> Result<DetailDto, String> {
    let items = state.items.lock().unwrap();
    let it = items
        .iter()
        .find(|it| it.id == id)
        .ok_or_else(|| "item not found".to_string())?;
    Ok(to_detail(it))
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

/// 写回剪贴板 + 推进 poller baseline（select/copy/batch_select 共用写入口径）
fn write_clipboard(state: &State<'_, AppState>, kind: &ItemKind) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    match kind {
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
    Ok(())
}

/// 隐藏面板；wait/paste 放到后台线程，不冻结 AppKit 主事件循环
/// （select_item / batch_select 共用投递路径）
fn hide_and_paste(app: &AppHandle, target_pid: i32) {
    if let Some(win) = app.get_webview_window("main") {
        panel_hide_ns(&win);
    }
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
}

#[tauri::command]
pub(crate) fn select_item(app: AppHandle, state: State<'_, AppState>, id: u64) -> Result<(), String> {
    // CGEventPost 模拟 Cmd+V 需要辅助功能权限；未授权时触发系统弹窗引导并中止本次粘贴
    if !ensure_ax() {
        return Err("NEED_AX_PERMISSION".into());
    }

    let item = take_item(&state, id)?;
    app.state::<storage::Storage>().request_save(); // 重排后持久化

    write_clipboard(&state, &item.kind)?;
    hide_and_paste(&app, state.prev_front_pid.load(Ordering::SeqCst));
    Ok(())
}

/// R19: 批量粘贴——多条文本按 "\n" 拼接写回剪贴板后 Cmd+V；选区全是图片时只取第一张
#[tauri::command]
pub(crate) fn batch_select(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Vec<u64>,
) -> Result<(), String> {
    if ids.is_empty() {
        return Err("empty selection".into());
    }
    // 与 select_item 同一权限口径：未授权触发系统弹窗引导并中止
    if !ensure_ax() {
        return Err("NEED_AX_PERMISSION".into());
    }

    let payload = {
        let mut items = state.items.lock().unwrap();
        let kind = crate::model::compose_batch(&items, &ids)
            .ok_or_else(|| "item not found".to_string())?;
        crate::model::promote_to_head(&mut items, &ids); // 与 take_item 一致的 recency 排序
        kind
    };
    app.state::<storage::Storage>().request_save(); // 重排后持久化

    write_clipboard(&state, &payload)?;
    hide_and_paste(&app, state.prev_front_pid.load(Ordering::SeqCst));
    Ok(())
}

/// R19: 批量删除
#[tauri::command]
pub(crate) fn batch_delete(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Vec<u64>,
) -> Result<(), String> {
    {
        let mut items = state.items.lock().unwrap();
        crate::model::remove_by_ids(&mut items, &ids);
    }
    app.state::<storage::Storage>().request_save();
    Ok(())
}

#[tauri::command]
pub(crate) fn copy_item(app: AppHandle, state: State<'_, AppState>, id: u64) -> Result<(), String> {
    let item = take_item(&state, id)?;
    app.state::<storage::Storage>().request_save(); // 重排后持久化
    write_clipboard(&state, &item.kind)
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

// ---------- 自定义背景（settings.json 的 background_image = 背景文件夹内文件的绝对路径） ----------

/// 背景媒体文件夹：app_data_dir/backgrounds（图片 + mp4/mov 视频，菜单动态列出）
/// 用户可自行往里丢文件；「选择图片/视频…」选中的文件也会拷贝进来统一管理
pub(crate) fn backgrounds_dir(app: &AppHandle) -> std::path::PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("backgrounds");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// 扩展名 → 是否视频扩展名（mp4/mov/m4v）
pub(crate) fn is_video_ext(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("mp4") | Some("mov") | Some("m4v")
    )
}

/// 是否支持的背景媒体（图片 png/jpg/jpeg/gif/webp/bmp/heic + 视频 mp4/mov/m4v）
pub(crate) fn is_supported_media(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("png")
            | Some("jpg")
            | Some("jpeg")
            | Some("gif")
            | Some("webp")
            | Some("bmp")
            | Some("heic")
            | Some("mp4")
            | Some("mov")
            | Some("m4v")
    )
}

/// 扫描背景文件夹：按文件名排序返回 (路径, 是否视频)；目录为空/不存在返回空 Vec
pub(crate) fn scan_backgrounds(app: &AppHandle) -> Vec<(std::path::PathBuf, bool)> {
    let dir = backgrounds_dir(app);
    let mut out: Vec<(std::path::PathBuf, bool)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_file() && is_supported_media(&p) {
                let video = is_video_ext(&p);
                out.push((p, video));
            }
        }
    }
    out.sort_by(|a, b| a.0.file_name().cmp(&b.0.file_name()));
    out
}

/// 把外部文件拷入背景文件夹（同名覆盖；已在文件夹内则原样返回），返回目标路径
pub(crate) fn import_background(
    app: &AppHandle,
    src: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let dir = backgrounds_dir(app);
    let name = src.file_name()?;
    let dst = dir.join(name);
    if src != dst {
        std::fs::copy(src, &dst).ok()?;
    }
    Some(dst)
}

/// 扩展名 → data URL 的 mime（未知扩展回退 png，WKWebView 对 mime 不匹配也能宽容渲染）
fn media_mime(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("heic") | Some("heif") => "image/heic",
        Some("svg") => "image/svg+xml",
        Some("mp4") | Some("m4v") => "video/mp4",
        Some("mov") => "video/quicktime",
        _ => "image/png",
    }
}

/// 自定义背景返回值（settings.json 的 background_image 为背景文件的绝对路径）：
/// kind = "image"（前端 <img> 渲染）| "video"（前端 <video autoplay loop muted> 渲染）
/// 缺失 / 文件不存在 / 解析失败均返回 None
#[derive(serde::Serialize)]
pub(crate) struct BackgroundInfo {
    #[serde(rename = "kind")] // "image" | "video"
    kind: &'static str,
    url: String,
}

/// 读取背景设置并返回结构化结果：
/// settings 值按绝对路径读文件，base64 后返回 data URL（图片与视频统一走 data URL：
/// 不动 tauri.conf.json 的 security 配置与 CSP，<video src=data:...> 在 WKWebView 可正常播放）。
/// 面板是持久 DOM，只在启动/切换时拉一次。
#[tauri::command]
pub(crate) fn get_background_image(app: AppHandle) -> Option<BackgroundInfo> {
    use base64::Engine;
    let dir = app.path().app_data_dir().ok()?;
    let bg = match crate::read_background_from_settings(&dir) {
        Some(s) => s,
        None => {
            eprintln!("[clipmate] get_background_image: no setting");
            return None;
        }
    };
    // 旧版 "preset:N" 语义已废弃（预设图移除，改为背景文件夹动态扫描）
    if bg.starts_with("preset:") {
        eprintln!("[clipmate] get_background_image: legacy preset spec ignored: {bg}");
        return None;
    }
    let p = std::path::Path::new(&bg);
    if !p.is_file() {
        eprintln!("[clipmate] background image missing: {}", p.display());
        return None;
    }
    let bytes = std::fs::read(p).ok()?;
    eprintln!(
        "[clipmate] background loaded {} bytes (video={})",
        bytes.len(),
        is_video_ext(p)
    );
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(BackgroundInfo {
        kind: if is_video_ext(p) { "video" } else { "image" },
        url: format!("data:{};base64,{}", media_mime(p), b64),
    })
}

#[tauri::command]
pub(crate) fn open_accessibility_settings() {
    // Windows 无辅助功能权限概念（SendInput 无需授权），空实现保持命令签名不变
    #[cfg(target_os = "macos")]
    {
        // 先触发一次系统请求弹窗（即使之前拒绝过也再试一次，让 ClipMate 出现在列表里）
        let _ = std::process::Command::new("osascript")
            .arg("-e")
            .arg(r#"tell application "System Events" to keystroke "" "#)
            .spawn();
        ax_trusted(true);
        // 同时打开辅助功能设置页（双 URL 兼容 macOS 13/15）
        crate::paste::open_accessibility_settings_page();
    }
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

// ---- 调试用：--bg-shot 模式让前端把 DOM 状态回传 Rust，打印到 eprintln ----
#[tauri::command]
pub(crate) fn bg_shot_report(report: String) {
    eprintln!("[bg-shot][report]\n{report}");
}
