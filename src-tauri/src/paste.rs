//! 粘贴模拟（CGEvent Cmd+V）+ AX 辅助功能（R7 从 main.rs 拆出，零行为变化）
//!
//! AX 约定（SPEC 红线 4）：只读——仅用于 caret 定位与授权检测，禁止 AXSet* 写操作。

use std::ffi::c_void;

use objc2::runtime::AnyObject;
use objc2_foundation::{NSPoint, NSRect};

pub(crate) const V_KEYCODE: u16 = 9; // virtual keycode for "V"

// ---------- paste simulation (CGEvent Cmd+V) ----------

pub(crate) fn paste_cmd_v() {
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
pub(crate) fn focused_element_frame() -> Option<(NSPoint, objc2_foundation::NSSize)> {
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
pub(crate) fn main_screen_size() -> (f64, f64) {
    unsafe {
        let Some(cls) = objc2::runtime::AnyClass::get(c"NSScreen") else { return (1512.0, 982.0) };
        let main: *mut AnyObject = objc2::msg_send![cls, mainScreen];
        let f: NSRect = objc2::msg_send![main, frame];
        (f.size.width, f.size.height)
    }
}

/// 主屏高度（points），用于 AX top-left 坐标 → AppKit bottom-left 坐标转换
pub(crate) fn main_screen_height() -> f64 {
    main_screen_size().1
}

pub(crate) fn ax_trusted(prompt: bool) -> bool {
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

/// 只弹一次系统授权框，避免每次选择都骚扰用户
pub(crate) fn ensure_ax() -> bool {
    static PROMPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if ax_trusted(false) {
        return true;
    }
    if !PROMPTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        ax_trusted(true);
    }
    false
}

/// 当前前台应用的 pid（用于把 Cmd+V 直接投递回原应用）
pub(crate) fn frontmost_pid() -> i32 {
    unsafe {
        let Some(cls) = objc2::runtime::AnyClass::get(c"NSWorkspace") else {
            return 0;
        };
        let ws: objc2::rc::Retained<AnyObject> = objc2::msg_send![cls, sharedWorkspace];
        let app: Option<objc2::rc::Retained<AnyObject>> = objc2::msg_send![&*ws, frontmostApplication];
        let Some(app) = app else { return 0 };
        let pid: i32 = objc2::msg_send![&*app, processIdentifier];
        pid
    }
}

/// 把指定 pid 的应用拉回前台（NSRunningApplication activate）
pub(crate) fn activate_app(pid: i32) -> bool {
    unsafe {
        let Some(cls) = objc2::runtime::AnyClass::get(c"NSRunningApplication") else {
            return false;
        };
        let app: Option<objc2::rc::Retained<AnyObject>> =
            objc2::msg_send![cls, runningApplicationWithProcessIdentifier: pid];
        let Some(app) = app else { return false };
        // NSApplicationActivateIgnoringOtherApps = 1 << 0
        let ok: bool = objc2::msg_send![&*app, activateWithOptions: 1isize];
        ok
    }
}
