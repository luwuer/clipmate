#![allow(dead_code)]

mod storage;
mod menubar;

use std::borrow::Cow;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use arboard::ImageData;
use base64::Engine;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use tauri_plugin_global_shortcut::ShortcutState;

const MAX_ITEMS: usize = 300;
const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024; // 2 MB
const MAX_PNG_BYTES: usize = 8 * 1024 * 1024; // 8 MB
const HOTKEY: &str = "F2";
const V_KEYCODE: u16 = 9; // virtual keycode for "V"
const PASTE_DELAY_MS: u64 = 150;

// ---------- data model ----------

#[derive(Clone)]
enum ItemKind {
    Text(String),
    Image { png: Vec<u8>, width: u32, height: u32 },
}

#[derive(Clone)]
struct ClipboardItem {
    id: u64,
    kind: ItemKind,
    created_at: u64, // unix ms
    pinned: bool,    // R2: 收藏/置顶，置顶项优先展示且防被新条目顶掉
}

#[derive(Serialize)]
struct ItemDto {
    id: u64,
    #[serde(rename = "type")]
    item_type: &'static str,
    text: Option<String>,
    image: Option<String>, // data url
    width: Option<u32>,
    height: Option<u32>,
    time: u64,
    pinned: bool, // R2
}

struct AppState {
    items: Mutex<Vec<ClipboardItem>>,
    next_id: AtomicU64,
    last_change_count: AtomicI64,
    positioned: AtomicBool,
    shown_at: Mutex<Option<Instant>>,
    /// 唤起面板前处于前台的 App，作为 Cmd+V 的投递目标
    prev_front_pid: AtomicI32,
}

impl AppState {
    fn new() -> Self {
        Self {
            items: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            last_change_count: AtomicI64::new(i64::MIN),
            positioned: AtomicBool::new(false),
            shown_at: Mutex::new(None),
            prev_front_pid: AtomicI32::new(0),
        }
    }
}

// ---------- macOS pasteboard change count (raw objc2) ----------

fn pasteboard_change_count() -> i64 {
    unsafe {
        let cls = objc2::runtime::AnyClass::get(c"NSPasteboard").expect("NSPasteboard class missing");
        let pb: Retained<AnyObject> = objc2::msg_send![cls, generalPasteboard];
        let count: isize = objc2::msg_send![&*pb, changeCount];
        count as i64
    }
}

/// 当前前台应用的 pid（用于把 Cmd+V 直接投递回原应用）
fn frontmost_pid() -> i32 {
    unsafe {
        let Some(cls) = objc2::runtime::AnyClass::get(c"NSWorkspace") else {
            return 0;
        };
        let ws: Retained<AnyObject> = objc2::msg_send![cls, sharedWorkspace];
        let app: Option<Retained<AnyObject>> = objc2::msg_send![&*ws, frontmostApplication];
        let Some(app) = app else { return 0 };
        let pid: i32 = objc2::msg_send![&*app, processIdentifier];
        pid
    }
}

/// 把指定 pid 的应用拉回前台（NSRunningApplication activate）
fn activate_app(pid: i32) -> bool {
    unsafe {
        let Some(cls) = objc2::runtime::AnyClass::get(c"NSRunningApplication") else {
            return false;
        };
        let app: Option<Retained<AnyObject>> =
            objc2::msg_send![cls, runningApplicationWithProcessIdentifier: pid];
        let Some(app) = app else { return false };
        // NSApplicationActivateIgnoringOtherApps = 1 << 0
        let ok: bool = objc2::msg_send![&*app, activateWithOptions: 1isize];
        ok
    }
}

// ---------- png helpers ----------

fn encode_png(img: &ImageData) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, img.width as u32, img.height as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(&img.bytes).ok()?;
    }
    Some(buf)
}

fn decode_png(bytes: &[u8]) -> Option<ImageData<'static>> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width, info.height);
    buf.truncate(w as usize * h as usize * 4);
    Some(ImageData {
        width: w as usize,
        height: h as usize,
        bytes: Cow::Owned(buf),
    })
}

fn same_content(a: &ClipboardItem, b: &ClipboardItem) -> bool {
    match (&a.kind, &b.kind) {
        (ItemKind::Text(x), ItemKind::Text(y)) => x == y,
        (
            ItemKind::Image { png: p1, width: w1, height: h1 },
            ItemKind::Image { png: p2, width: w2, height: h2 },
        ) => p1 == p2 && w1 == w2 && h1 == h2,
        _ => false,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------- clipboard poller ----------

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
        let png = encode_png(&img)?;
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

fn start_poller(app: AppHandle) {
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
                if items.first().map(|it| same_content(it, &item)) == Some(true) {
                    continue; // duplicate of the most recent entry
                }
                items.insert(0, item);
                if items.len() > MAX_ITEMS {
                    items.truncate(MAX_ITEMS);
                }
                drop(items);
                app.state::<storage::Storage>().request_save();
                let _ = app.emit("history-changed", ());
            }
        }
    });
}

// ---------- paste simulation (CGEvent Cmd+V) ----------

fn paste_cmd_v() {
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    let Ok(src) = CGEventSource::new(CGEventSourceStateID::CombinedSessionState) else {
        return;
    };
    // 统一走 HID tap：post_to_pid 对 Secure Input / Electron / 游戏应用会静默失败，
    // 而面板从不抢焦点，目标应用始终是 frontmost，HID tap 投递最稳。
    for key_down in [true, false] {
        if let Ok(ev) = CGEvent::new_keyboard_event(src.clone(), V_KEYCODE, key_down) {
            ev.set_flags(CGEventFlags::CGEventFlagCommand);
            ev.post(CGEventTapLocation::HID);
        }
    }
}

// ---------- accessibility permission & AX caret query ----------

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    fn AXUIElementCreateSystemWide() -> *mut AnyObject;
    fn AXUIElementCopyAttributeValue(
        element: *mut AnyObject,
        attribute: core_foundation::string::CFStringRef,
        value: *mut core_foundation::base::CFTypeRef,
    ) -> i32;
    fn AXValueGetValue(
        value: *const AnyObject,
        the_type: u32,
        value_ptr: *mut c_void,
    ) -> bool;
}

/// 取「前台应用焦点元素」的 frame（即输入光标所在控件），AX 坐标系为
/// 全局 top-left origin；失败返回 None（回退鼠标位置）。
fn focused_element_frame() -> Option<(NSPoint, objc2_foundation::NSSize)> {
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;

    unsafe {
        let sw = AXUIElementCreateSystemWide();
        if sw.is_null() {
            return None;
        }
        let mut focused: core_foundation::base::CFTypeRef = std::ptr::null();
        let attr = CFString::new("AXFocusedUIElement");
        if AXUIElementCopyAttributeValue(sw, attr.as_concrete_TypeRef(), &mut focused) != 0
            || focused.is_null()
        {
            return None;
        }
        let focused = focused as *mut AnyObject;

        // AXPosition（AXValue CGPoint，type = 1）
        let mut pos_v: core_foundation::base::CFTypeRef = std::ptr::null();
        let pattr = CFString::new("AXPosition");
        if AXUIElementCopyAttributeValue(focused, pattr.as_concrete_TypeRef(), &mut pos_v) != 0
            || pos_v.is_null()
        {
            return None;
        }
        let mut pt = core_graphics::geometry::CGPoint { x: 0.0, y: 0.0 };
        if !AXValueGetValue(
            pos_v as *const AnyObject,
            1, // kAXValueCGPointType
            &mut pt as *mut core_graphics::geometry::CGPoint as *mut c_void,
        ) {
            return None;
        }

        // AXSize（可失败；输入光标本身没有尺寸时 size 为 0）
        let mut sz = core_graphics::geometry::CGSize { width: 0.0, height: 0.0 };
        let mut size_v: core_foundation::base::CFTypeRef = std::ptr::null();
        let sattr = CFString::new("AXSize");
        if AXUIElementCopyAttributeValue(focused, sattr.as_concrete_TypeRef(), &mut size_v) == 0
            && !size_v.is_null()
        {
            let _ = AXValueGetValue(
                size_v as *const AnyObject,
                2, // kAXValueCGSizeType
                &mut sz as *mut core_graphics::geometry::CGSize as *mut c_void,
            );
        }

        Some((
            NSPoint { x: pt.x, y: pt.y },
            objc2_foundation::NSSize { width: sz.width, height: sz.height },
        ))
    }
}

/// 主屏尺寸（points），用于 AX top-left 坐标 → AppKit bottom-left 坐标转换
fn main_screen_size() -> (f64, f64) {
    unsafe {
        let Some(cls) = objc2::runtime::AnyClass::get(c"NSScreen") else { return (1512.0, 982.0) };
        let main: *mut AnyObject = objc2::msg_send![cls, mainScreen];
        let f: NSRect = objc2::msg_send![main, frame];
        (f.size.width, f.size.height)
    }
}

/// 主屏高度（points），用于 AX top-left 坐标 → AppKit bottom-left 坐标转换
fn main_screen_height() -> f64 {
    main_screen_size().1
}

fn ax_trusted(prompt: bool) -> bool {
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;

    unsafe {
        if !prompt {
            return AXIsProcessTrustedWithOptions(std::ptr::null());
        }
        let key = CFString::new("AXTrustedCheckOptionPrompt");
        let value = CFBoolean::true_value();
        let dict = CFDictionary::from_CFType_pairs(&[(key, value)]);
        AXIsProcessTrustedWithOptions(dict.as_concrete_TypeRef() as *const c_void)
    }
}

// ---------- panel control ----------

use objc2_foundation::{NSPoint, NSRect};

// ---------- non-activating NSPanel（不抢焦点的面板，CleanClip 同款方案） ----------

extern "C" {
    fn object_setClass(obj: *mut AnyObject, cls: *const AnyObject) -> *mut AnyObject;
    fn object_getClass(obj: *mut AnyObject) -> *const AnyObject;
    fn class_getName(cls: *const AnyObject) -> *const std::os::raw::c_char;
    fn class_replaceMethod(
        cls: *const AnyObject,
        name: *const std::ffi::c_void,
        imp: unsafe extern "C" fn(*mut AnyObject, *const std::ffi::c_void) -> u8,
        types: *const std::os::raw::c_char,
    ) -> *mut std::ffi::c_void;
    fn sel_registerName(name: *const std::os::raw::c_char) -> *const std::ffi::c_void;
}

/// 强制 canBecomeKeyWindow = YES：borderless nonactivating NSPanel 默认返回 NO，
/// 导致 makeKeyAndOrderFront 静默失败、键盘事件全部进不了面板（本轮诊断确认的根因）。
unsafe extern "C" fn panel_can_become_key(
    _self: *mut AnyObject,
    _cmd: *const std::ffi::c_void,
) -> u8 {
    let _ = (_self, _cmd);
    1 // YES
}

/// 递归查找 WKWebView —— 真正的键盘接收者，藏在 contentView 子视图树里
unsafe fn find_webview_view(view: *mut AnyObject) -> Option<*mut AnyObject> {
    if view.is_null() {
        return None;
    }
    let cls = object_getClass(view);
    if !cls.is_null() {
        let name = class_getName(cls);
        if !name.is_null() {
            let bytes = std::ffi::CStr::from_ptr(name).to_bytes();
            if bytes.windows(8).any(|w| w == b"WKWebView") {
                return Some(view);
            }
        }
    }
    let subs: *mut AnyObject = objc2::msg_send![view, subviews];
    let count: usize = objc2::msg_send![subs, count];
    for i in 0..count {
        let sub: *mut AnyObject = objc2::msg_send![subs, objectAtIndex: i];
        if let Some(found) = find_webview_view(sub) {
            return Some(found);
        }
    }
    None
}

/// 把 Tauri 的 borderless NSWindow 强转为 NSPanel 并打开 NonactivatingPanel 风格。
/// 之后面板可以成为 key window（接收键盘）但**不会激活本应用**，
/// 目标应用始终保持前台 —— Cmd+V 不再有任何焦点交接时序问题。
fn convert_to_panel(win: &WebviewWindow) {
    unsafe {
        let Ok(ptr) = win.ns_window() else { return };
        let ns_win = ptr as *mut AnyObject;
        let Some(panel_cls) = objc2::runtime::AnyClass::get(c"NSPanel") else { return };
        object_setClass(ns_win, (panel_cls as *const objc2::runtime::AnyClass).cast::<AnyObject>());
        // NSWindowStyleMaskNonactivatingPanel = 0x80
        let style: isize = objc2::msg_send![ns_win, styleMask];
        let _: () = objc2::msg_send![ns_win, setStyleMask: style | 0x80];
        // 关键：borderless nonactivating panel 默认 canBecomeKeyWindow=NO，
        // 强制替换为 YES，否则 makeKeyAndOrderFront 静默失败（键盘全挂）
        let sel = sel_registerName(c"canBecomeKeyWindow".as_ptr());
        let types: &'static [u8] = b"B@:\0";
        class_replaceMethod(
            (panel_cls as *const objc2::runtime::AnyClass).cast::<AnyObject>(),
            sel,
            panel_can_become_key,
            types.as_ptr() as *const std::os::raw::c_char,
        );
        let _: () = objc2::msg_send![ns_win, setHidesOnDeactivate: false];
        let _: () = objc2::msg_send![ns_win, setWorksWhenModal: true];
        let _: () = objc2::msg_send![ns_win, setBecomesKeyOnlyIfNeeded: false];
        // NSFloatingWindowLevel，保证盖在普通窗口上
        let _: () = objc2::msg_send![ns_win, setLevel: 3isize];
        // 让面板在全屏应用上方也能看见：CanJoinAllSpaces(1) | FullScreenAuxiliary(32)
        let _: () = objc2::msg_send![ns_win, setCollectionBehavior: 33usize];
    }
}

/// 面板定位：优先出现在「输入光标（AX 焦点元素）」正下方（CleanClip 行为），
/// 拿不到焦点元素时回退到鼠标位置；并 clamp 在所在屏幕的可视区域内。
fn position_under_cursor(win: &WebviewWindow) {
    unsafe {
        let Ok(ptr) = win.ns_window() else { return };
        let ns_win = ptr as *mut AnyObject;

        // ---- 锚点：优先 AX 焦点元素（输入光标所在控件），回退鼠标 ----
        let Some(nsevent_cls) = objc2::runtime::AnyClass::get(c"NSEvent") else { return };
        let mouse: NSPoint = objc2::msg_send![nsevent_cls, mouseLocation];

        // 焦点元素 frame 合理性过滤：某些 app（Electron/自绘 UI）的 AXFocusedUIElement
        // 会返回整窗/超大区域甚至 (0,0)，此时定位不可靠 → 回退鼠标位置
        let caret_anchor = focused_element_frame().and_then(|(pt, sz)| {
            let (mw, mh) = main_screen_size();
            let plausible = (pt.x > 1.0 || pt.y > 1.0) // 非全零
                && pt.x >= -mw && pt.x <= 2.0 * mw
                && pt.y >= -mh && pt.y <= 2.0 * mh
                && sz.width > 0.0
                && sz.width <= mw * 0.9 // 不超过九成屏宽（整窗判定）
                && sz.height <= mh * 0.9;
            if !plausible {
                return None;
            }
            // AX 坐标（全局 top-left origin）→ AppKit（bottom-left origin）
            let elem_bottom_appkit = mh - pt.y - sz.height;
            Some((pt.x + sz.width / 2.0, elem_bottom_appkit))
        });

        let (anchor_x, anchor_y) = caret_anchor.unwrap_or((mouse.x, mouse.y));
        let (anchor_x, anchor_y) = caret_anchor.unwrap_or((mouse.x, mouse.y));

        let frame: NSRect = objc2::msg_send![ns_win, frame];

        // 找锚点所在的屏幕（默认主屏），用 visibleFrame 避开菜单栏和 Dock
        let Some(screen_cls) = objc2::runtime::AnyClass::get(c"NSScreen") else { return };
        let main: *mut AnyObject = objc2::msg_send![screen_cls, mainScreen];
        let mut vis: NSRect = objc2::msg_send![main, visibleFrame];
        let screens: *mut AnyObject = objc2::msg_send![screen_cls, screens];
        let count: usize = objc2::msg_send![screens, count];
        for i in 0..count {
            let scr: *mut AnyObject = objc2::msg_send![screens, objectAtIndex: i];
            let full: NSRect = objc2::msg_send![scr, frame];
            if anchor_x >= full.origin.x
                && anchor_x <= full.origin.x + full.size.width
                && anchor_y >= full.origin.y
                && anchor_y <= full.origin.y + full.size.height
            {
                vis = objc2::msg_send![scr, visibleFrame];
                break;
            }
        }

        // 水平：以锚点为中心，clamp 到可视区
        let min_x = vis.origin.x + 8.0;
        let max_x = vis.origin.x + vis.size.width - frame.size.width - 8.0;
        let x = if max_x < min_x {
            min_x
        } else {
            (anchor_x - frame.size.width / 2.0).clamp(min_x, max_x)
        };

        // 垂直：锚点下方 16pt；若超出屏幕底部则改放锚点上方
        let gap = 16.0;
        let mut top_y = anchor_y - gap;
        if top_y - frame.size.height < vis.origin.y + 8.0 {
            top_y = (anchor_y + gap + frame.size.height).min(vis.origin.y + vis.size.height - 8.0);
        }

        let _: () = objc2::msg_send![ns_win, setFrameTopLeftPoint: NSPoint { x, y: top_y }];
    }
}

/// 非激活方式显示面板：makeKeyAndOrderFront（nonactivating panel 不激活 NSApp），
/// 然后通过 tauri::Webview::set_focus() 把 WKWebView 设为 first responder
/// （wry 内部走 makeFirstResponder，不激活 NSApp）。
/// 保留 NSEvent local monitor 兜底处理 ESC/方向键/Enter（不依赖 DOM focus）。
fn panel_show_ns(win: &WebviewWindow) {
    unsafe {
        let Ok(ptr) = win.ns_window() else { return };
        let ns_win = ptr as *mut AnyObject;
        let _: () = objc2::msg_send![ns_win, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
        // 诊断日志：帮助定位键盘链路问题
        let is_key: bool = objc2::msg_send![ns_win, isKeyWindow];
        let fr: *mut AnyObject = objc2::msg_send![ns_win, firstResponder];
        let mut fr_name = std::string::String::from("nil");
        if !fr.is_null() {
            let cls = object_getClass(fr);
            if !cls.is_null() {
                let name = class_getName(cls);
                if !name.is_null() {
                    fr_name = std::ffi::CStr::from_ptr(name).to_string_lossy().into_owned();
                }
            }
        }
        eprintln!("[clipmate] panel shown: isKeyWindow={is_key} firstResponder={fr_name}");
    }
    // wry 的 focus() = window.makeFirstResponder(&wkwebview)，不会激活 app
    let webview: &tauri::Webview<tauri::Wry> = win.as_ref();
    let _ = webview.set_focus();
}

/// 隐藏面板（orderOut，不动焦点）
fn panel_hide_ns(win: &WebviewWindow) {
    unsafe {
        let Ok(ptr) = win.ns_window() else { return };
        let ns_win = ptr as *mut AnyObject;
        let _: () = objc2::msg_send![ns_win, orderOut: std::ptr::null::<AnyObject>()];
    }
}

fn show_panel(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else { return };
    // 面板从不抢焦点，但仍然记录 frontmost 作为粘贴目标（双保险）
    let pid = frontmost_pid();
    if pid > 0 {
        win.state::<AppState>().prev_front_pid.store(pid, Ordering::SeqCst);
    }
    position_under_cursor(&win);
    *win.state::<AppState>().shown_at.lock().unwrap() = Some(Instant::now());
    panel_show_ns(&win);
    let _ = app.emit("panel-shown", ());
}

// R3: 供 menubar.rs 回调使用（菜单"显示/隐藏剪贴板面板"）
pub fn toggle_panel_pub(app: &AppHandle) { toggle_panel(app); }

fn toggle_panel(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else { return };
    if win.is_visible().unwrap_or(false) {
        panel_hide_ns(&win);
    } else {
        show_panel(app);
    }
}

// ---------- tauri commands ----------

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
fn get_history(state: State<'_, AppState>, query: String) -> Vec<ItemDto> {
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
fn select_item(app: AppHandle, state: State<'_, AppState>, id: u64) -> Result<(), String> {
    // CGEventPost 模拟 Cmd+V 需要辅助功能权限；未授权时触发系统弹窗引导并中止本次粘贴
    if !ensure_ax() {
        return Err("NEED_AX_PERMISSION".into());
    }

    let item = take_item(&state, id)?;
    app.state::<storage::Storage>().request_save(); // 重排后持久化
    let target_pid = state.prev_front_pid.load(Ordering::SeqCst);

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
fn copy_item(app: AppHandle, state: State<'_, AppState>, id: u64) -> Result<(), String> {
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
    state
        .last_change_count
        .store(pasteboard_change_count(), Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
fn delete_item(app: AppHandle, state: State<'_, AppState>, id: u64) -> Result<(), String> {
    let mut items = state.items.lock().unwrap();
    items.retain(|it| it.id != id);
    drop(items);
    app.state::<storage::Storage>().request_save();
    Ok(())
}

#[tauri::command]
fn clear_history(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    state.items.lock().unwrap().clear();
    app.state::<storage::Storage>().request_save(); // 清空后落盘 → jsonl 重写为空
    Ok(())
}

#[tauri::command]
fn toggle_pin(app: AppHandle, state: State<'_, AppState>, id: u64) -> Result<bool, String> {
    let mut items = state.items.lock().unwrap();
    let it = items.iter_mut().find(|it| it.id == id).ok_or_else(|| "item not found".to_string())?;
    it.pinned = !it.pinned;
    let new_state = it.pinned;
    drop(items);
    app.state::<storage::Storage>().request_save();
    Ok(new_state)
}

#[tauri::command]
fn hide_panel(app: AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        panel_hide_ns(&win);
    }
}

#[tauri::command]
fn is_ax_trusted() -> bool {
    ax_trusted(false)
}

#[tauri::command]
fn open_accessibility_settings() {
    let _ = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .spawn();
}

// ---------- main ----------

use std::ptr::NonNull;

use block2::RcBlock;

/// 只弹一次系统授权框，避免每次选择都骚扰用户
fn ensure_ax() -> bool {
    static PROMPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if ax_trusted(false) {
        return true;
    }
    if !PROMPTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        ax_trusted(true);
    }
    false
}

// ---------- 点击面板外部时自动关闭（NSEvent global mouse monitor，无需权限） ----------

fn install_mouse_monitor(app: &AppHandle) {
    let app_handle = app.clone();
    let handler = RcBlock::new(move |_e: NonNull<AnyObject>| {
        let Some(win) = app_handle.get_webview_window("main") else { return };
        if !win.is_visible().unwrap_or(false) {
            return;
        }
        // 刚显示的 150ms 内忽略，避免误触
        let recently_shown = win
            .state::<AppState>()
            .shown_at
            .lock()
            .unwrap()
            .map(|t| t.elapsed() < Duration::from_millis(150))
            .unwrap_or(false);
        if recently_shown {
            return;
        }
        unsafe {
            let Some(cls) = objc2::runtime::AnyClass::get(c"NSEvent") else { return };
            let mouse: NSPoint = objc2::msg_send![cls, mouseLocation];
            let Ok(ptr) = win.ns_window() else { return };
            let ns_win = ptr as *mut AnyObject;
            let frame: NSRect = objc2::msg_send![ns_win, frame];
            let inside = mouse.x >= frame.origin.x
                && mouse.x <= frame.origin.x + frame.size.width
                && mouse.y >= frame.origin.y
                && mouse.y <= frame.origin.y + frame.size.height;
            if !inside {
                panel_hide_ns(&win);
            }
        }
    });
    let block = unsafe { RcBlock::copy(RcBlock::as_ptr(&handler)) }.unwrap();
    unsafe {
        let Some(cls) = objc2::runtime::AnyClass::get(c"NSEvent") else { return };
        // leftMouseDown(1<<0) | rightMouseDown(1<<1) | otherMouseDown(1<<2)
        let _: () = objc2::msg_send![
            cls,
            addGlobalMonitorForEventsMatchingMask: 7isize,
            handler: &*block
        ];
    }
}

#[tauri::command]
fn copy_tccutil_command(state: State<'_, AppState>) -> Result<(), String> {
    let cmd = "tccutil reset Accessibility com.mdy.clipmate";
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_text(cmd.to_string()).map_err(|e| e.to_string())?;
    // 推进 baseline 防止 poller 把这条命令文本作为历史记录
    state
        .last_change_count
        .fetch_max(pasteboard_change_count(), Ordering::SeqCst);
    Ok(())
}

fn main() {
    eprintln!("[clipmate] starting…");
    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        toggle_panel(app);
                    }
                })
                .build(),
        )
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            get_history,
            select_item,
            copy_item,
            delete_item,
            clear_history,
            toggle_pin,
            hide_panel,
            is_ax_trusted,
            open_accessibility_settings,
            copy_tccutil_command
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // ---- 历史持久化：加载既有条目 + 启动防抖落盘线程 ----
            let data_dir = app.path().app_data_dir().expect("app_data_dir unavailable");
            let storage = storage::Storage::new(data_dir);
            let loaded = storage.load();
            let n_loaded = loaded.len();
            if n_loaded > 0 {
                let state = app.state::<AppState>();
                let max_id = loaded.iter().map(|it| it.id).max().unwrap_or(0);
                state.next_id.store(max_id + 1, Ordering::SeqCst); // 防 id 冲突
                *state.items.lock().unwrap() = loaded;
            }
            eprintln!("[clipmate] loaded {n_loaded} persisted items");
            app.manage(storage.clone());
            storage.start_flusher(app.handle().clone());

            // register the F2 hotkey
            use tauri_plugin_global_shortcut::GlobalShortcutExt;
            match app.global_shortcut().register(HOTKEY) {
                Ok(_) => eprintln!("[clipmate] hotkey {HOTKEY} registered"),
                Err(e) => eprintln!("[clipmate] hotkey register failed: {e}"),
            }

            // 把窗口转成「不激活应用」的 NSPanel —— 全程不抢目标应用焦点
            if let Some(win) = app.get_webview_window("main") {
                convert_to_panel(&win);

                // 点击面板外时自动隐藏（面板 resign key / 应用失焦都覆盖）
                let win_clone = win.clone();
                win.on_window_event(move |event| {
                    if let tauri::WindowEvent::Focused(false) = event {
                        let state = win_clone.state::<AppState>();
                        let recently_shown = state
                            .shown_at
                            .lock()
                            .unwrap()
                            .map(|t| t.elapsed() < Duration::from_millis(150))
                            .unwrap_or(false);
                        if !recently_shown {
                            panel_hide_ns(&win_clone);
                        }
                    }
                });
            }

            start_poller(app.handle().clone());
            install_mouse_monitor(&app.handle());
            // R3: 安装菜单栏图标 + 退出入口（AppHandle 按值传入，Box 泄漏存全局）
            menubar::install(app.handle().clone());

            // 启动时若无辅助功能权限，触发一次系统授权弹窗（粘贴功能必需：
            // CGEventPost 模拟 Cmd+V 在 macOS 10.14+ 需要该权限，否则事件被静默丢弃）
            let _ = std::thread::spawn(|| {
                std::thread::sleep(Duration::from_millis(500));
                if !ax_trusted(false) {
                    ax_trusted(true);
                }
            });

            eprintln!("[clipmate] setup complete");

            if std::env::args().any(|a| a == "--show") {
                show_panel(&app.handle().clone());
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running ClipMate");
}
