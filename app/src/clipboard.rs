//! 剪贴板监听与捕获（R7 从 main.rs 拆出，零行为变化）
//! NSPasteboard changeCount 轮询(200ms) → capture → 去重入库(enforce_limit)
//!
//! W1 Windows 移植：pasteboard_change_count 换用 GetClipboardSequenceNumber，
//! 语义与 macOS changeCount 完全一致（剪贴板每次变化单调递增），轮询/去重逻辑零改动。

use std::sync::atomic::Ordering;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};

use crate::model::{
    AppState, ClipboardItem, ItemKind, MAX_PNG_BYTES, MAX_TEXT_BYTES, insert_dedup, now_ms,
};
use crate::storage;

// ---------- 剪贴板变化计数（平台实现） ----------

/// macOS：NSPasteboard changeCount（raw objc2）
#[cfg(target_os = "macos")]
pub(crate) fn pasteboard_change_count() -> i64 {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;

    unsafe {
        let cls = objc2::runtime::AnyClass::get(c"NSPasteboard").expect("NSPasteboard class missing");
        let pb: Retained<AnyObject> = objc2::msg_send![cls, generalPasteboard];
        let count: isize = objc2::msg_send![&*pb, changeCount];
        count as i64
    }
}

/// Windows：GetClipboardSequenceNumber（同样单调递增，无需打开剪贴板）
#[cfg(target_os = "windows")]
pub(crate) fn pasteboard_change_count() -> i64 {
    unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() as i64 }
}

fn capture_item(state: &AppState, clipboard: &mut arboard::Clipboard) -> Option<ClipboardItem> {
    let id = state.next_id.fetch_add(1, Ordering::SeqCst);
    let created_at = now_ms();

    if let Ok(text) = clipboard.get_text() {
        if text.trim().is_empty() || text.len() > MAX_TEXT_BYTES {
            return None;
        }
        return Some(ClipboardItem { id, kind: ItemKind::Text(text), created_at, pinned: false });
    }

    if let Ok(img) = clipboard.get_image() {
        let png = crate::model::encode_png(&img)?;
        if png.len() > MAX_PNG_BYTES {
            return None;
        }
        return Some(ClipboardItem {
            id,
            kind: ItemKind::Image { png, width: img.width as u32, height: img.height as u32 },
            created_at,
            pinned: false,
        });
    }

    None
}

pub(crate) fn start_poller(app: AppHandle) {
    std::thread::spawn(move || {
        let mut clipboard = match arboard::Clipboard::new() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("clipboard init failed: {e}");
                return;
            }
        };
        let state = app.state::<AppState>();
        // baseline: don't record whatever is on the pasteboard at launch
        state
            .last_change_count
            .store(pasteboard_change_count(), Ordering::SeqCst);

        loop {
            std::thread::sleep(Duration::from_millis(200));
            let count = pasteboard_change_count();
            if count == state.last_change_count.load(Ordering::SeqCst) {
                continue;
            }
            let new_item = capture_item(&state, &mut clipboard);
            // 单调递增推进 baseline —— 防止 select_item 推进的值被 poller 的旧值回退
            state.last_change_count.fetch_max(count, Ordering::SeqCst);

            if let Some(item) = new_item {
                let mut items = state.items.lock().unwrap();
                if !insert_dedup(&mut items, item) {
                    continue; // 命中 pinned 去重，历史无变化
                }
                drop(items);
                app.state::<storage::Storage>().request_save();
                let _ = app.emit("history-changed", ());
            }
        }
    });
}
