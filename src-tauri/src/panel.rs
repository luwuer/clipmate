//! non-activating NSPanel 面板控制（R7 从 main.rs 拆出，零行为变化）
//!
//! 焦点模型是键盘工作的根基（SPEC 红线 3）：canBecomeKeyWindow swizzle、
//! styleMask |= 0x80、orderFrontRegardless/makeKeyAndOrderFront 路径、
//! WKWebView makeFirstResponder——只能加固不能绕开。

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use objc2::runtime::AnyObject;
use objc2_foundation::{NSPoint, NSRect};

use tauri::{AppHandle, Emitter, Manager, WebviewWindow};

use crate::model::AppState;
use crate::paste::{focused_element_frame, frontmost_pid, main_screen_size};

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
/// 导致 makeKeyAndOrderFront 静默失败、键盘事件全部进不了面板（历史诊断确认的根因）。
unsafe extern "C" fn panel_can_become_key(
    _self: *mut AnyObject,
    _cmd: *const std::ffi::c_void,
) -> u8 {
    let _ = (_self, _cmd);
    1 // YES
}

/// 把 Tauri 的 borderless NSWindow 强转为 NSPanel 并打开 NonactivatingPanel 风格。
/// 之后面板可以成为 key window（接收键盘）但**不会激活本应用**，
/// 目标应用始终保持前台 —— Cmd+V 不再有任何焦点交接时序问题。
pub(crate) fn convert_to_panel(win: &WebviewWindow) {
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
pub(crate) fn position_under_cursor(win: &WebviewWindow) {
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

        // 测试模式：CLIPMATE_TEST_CENTER=1 强制 panel 在主屏中央 1/4 位置（便于人眼/截屏定位验证）
        let (anchor_x, anchor_y) = if std::env::var("CLIPMATE_TEST_CENTER").is_ok() {
            let (mw, mh) = main_screen_size();
            (mw / 2.0, mh - 80.0)
        } else {
            caret_anchor.unwrap_or((mouse.x, mouse.y))
        };

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
pub(crate) fn panel_show_ns(win: &WebviewWindow) {
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
pub(crate) fn panel_hide_ns(win: &WebviewWindow) {
    unsafe {
        let Ok(ptr) = win.ns_window() else { return };
        let ns_win = ptr as *mut AnyObject;
        let _: () = objc2::msg_send![ns_win, orderOut: std::ptr::null::<AnyObject>()];
    }
}

pub(crate) fn show_panel(app: &AppHandle) {
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

pub(crate) fn toggle_panel(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else { return };
    if win.is_visible().unwrap_or(false) {
        panel_hide_ns(&win);
    } else {
        show_panel(app);
    }
}

// R3: 供 menubar.rs 回调使用（菜单"显示/隐藏剪贴板面板"）
pub fn toggle_panel_pub(app: &AppHandle) {
    toggle_panel(app);
}

// ---------- 点击面板外部时自动关闭（NSEvent global mouse monitor，无需权限） ----------

pub(crate) fn install_mouse_monitor(app: &AppHandle) {
    use std::ptr::NonNull;

    use block2::RcBlock;

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
