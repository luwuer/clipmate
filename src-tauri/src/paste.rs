//! 粘贴模拟 + 焦点管理（R7 从 main.rs 拆出，零行为变化）
//!
//! 平台实现（W1 Windows 移植）：
//! - macOS：CGEvent Cmd+V + AX 辅助功能（只读，SPEC 红线 4）
//! - Windows：SendInput Ctrl+V；无辅助功能权限概念，ax_* 恒为 true
//!
//! 对外接口两个平台完全一致（commands.rs / panel.rs / menubar.rs 零改动）。

#[cfg(target_os = "macos")]
mod macos_impl {
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
        fn AXUIElementCopyParameterizedAttributeValue(
            element: *mut AnyObject,
            attribute: core_foundation::string::CFStringRef,
            parameter: core_foundation::base::CFTypeRef,
            value: *mut core_foundation::base::CFTypeRef,
        ) -> i32;
        fn AXValueCreate(
            the_type: u32,
            value_ptr: *const c_void,
        ) -> core_foundation::base::CFTypeRef;
        fn AXValueGetValue(
            value: *const AnyObject,
            the_type: u32,
            value_ptr: *mut c_void,
        ) -> bool;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: *const c_void);
    }

    /// CFRange（kAXValueCFRangeType = 4 对应的载荷），CFIndex = long = isize
    #[repr(C)]
    struct AXCFRange {
        location: isize,
        length: isize,
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

    /// 取「插入点（caret）」的精确屏幕 frame，比 focused_element_frame 更细：
    /// 直接定位到文本框内光标所在位置，而不是整个输入控件的外框。
    ///
    /// 链路（全部 AX 只读）：
    ///   systemWide → AXFocusedUIElement
    ///   → AXSelectedTextRange（AXValue CFRange，光标=空选区即插入点）
    ///   → AXBoundsForRange（parameterized attribute，参数为上述 CFRange 的 AXValue）
    ///   → 返回该范围的屏幕 bounds（AXValue CGRect，空选区时就是 caret 的细条矩形）
    ///
    /// 坐标系说明：AX 返回的是**全局 top-left origin** 坐标（y 向下增长）；
    /// AppKit（NSScreen/NSWindow）用 **bottom-left origin**（y 向上增长）。
    /// 本函数返回原始 AX 坐标，y 轴翻转统一由调用方完成：
    ///   appkit_y = 主屏高 - ax_y - rect_height
    /// （多屏环境下 AX/CG 全局坐标的原点就是主屏左上角，故用主屏高翻转即可。）
    ///
    /// 任一步失败返回 None（调用方依次回退：元素 frame → 鼠标位置）。
    pub(crate) fn caret_precise_frame() -> Option<(NSPoint, objc2_foundation::NSSize)> {
        use core_foundation::base::{CFTypeRef, TCFType};
        use core_foundation::string::CFString;

        const K_AX_VALUE_CGRECT_TYPE: u32 = 3;
        const K_AX_VALUE_CF_RANGE_TYPE: u32 = 4;

        unsafe {
            let sw = AXUIElementCreateSystemWide();
            if sw.is_null() {
                return None;
            }
            let mut focused: CFTypeRef = std::ptr::null();
            let attr = CFString::new("AXFocusedUIElement");
            if AXUIElementCopyAttributeValue(sw, attr.as_concrete_TypeRef(), &mut focused) != 0
                || focused.is_null()
            {
                return None;
            }
            let focused = focused as *mut AnyObject;

            // 选中范围（光标 = 空选区）。部分自绘/Electron 控件不支持该属性 → 失败即回退。
            let mut range_v: CFTypeRef = std::ptr::null();
            let rattr = CFString::new("AXSelectedTextRange");
            if AXUIElementCopyAttributeValue(focused, rattr.as_concrete_TypeRef(), &mut range_v) != 0
                || range_v.is_null()
            {
                return None;
            }
            let mut range = AXCFRange { location: 0, length: 0 };
            if !AXValueGetValue(
                range_v as *const AnyObject,
                K_AX_VALUE_CF_RANGE_TYPE,
                &mut range as *mut AXCFRange as *mut c_void,
            ) {
                CFRelease(range_v);
                return None;
            }
            CFRelease(range_v);

            // 把 CFRange 包成 AXValue 作为 parameterized attribute 的参数（create 规则，用完释放）
            let range_param = AXValueCreate(
                K_AX_VALUE_CF_RANGE_TYPE,
                &range as *const AXCFRange as *const c_void,
            );
            if range_param.is_null() {
                return None;
            }
            let battr = CFString::new("AXBoundsForRange");
            let mut bounds_v: CFTypeRef = std::ptr::null();
            let err = AXUIElementCopyParameterizedAttributeValue(
                focused,
                battr.as_concrete_TypeRef(),
                range_param,
                &mut bounds_v,
            );
            CFRelease(range_param);
            if err != 0 || bounds_v.is_null() {
                return None;
            }

            // bounds 是 AXValue 包装的 CGRect（全局 top-left origin 屏幕坐标）
            let mut rect = core_graphics::geometry::CGRect {
                origin: core_graphics::geometry::CGPoint { x: 0.0, y: 0.0 },
                size: core_graphics::geometry::CGSize { width: 0.0, height: 0.0 },
            };
            let ok = AXValueGetValue(
                bounds_v as *const AnyObject,
                K_AX_VALUE_CGRECT_TYPE,
                &mut rect as *mut core_graphics::geometry::CGRect as *mut c_void,
            );
            CFRelease(bounds_v);
            if !ok {
                return None;
            }

            Some((
                NSPoint { x: rect.origin.x, y: rect.origin.y },
                objc2_foundation::NSSize { width: rect.size.width, height: rect.size.height },
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

    /// 打开「系统设置 → 隐私与安全性 → 辅助功能」设置页（双 URL 兼容 macOS 13/15）
    pub(crate) fn open_accessibility_settings_page() {
        let urls = [
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Accessibility",
        ];
        for url in urls {
            let _ = std::process::Command::new("open").arg(url).spawn();
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }

    /// 每次无权限都重新触发系统弹窗 + 打开设置页兜底。
    /// 注意：AXIsProcessTrustedWithOptions(prompt:true) 在用户拒绝过一次后，
    /// 系统自己也不会重复弹框（TCC 行为）——所以必须同时打开设置页，
    /// 让用户无论系统弹不弹框都总能找到授权入口。
    pub(crate) fn ensure_ax() -> bool {
        if ax_trusted(false) {
            return true;
        }
        ax_trusted(true);
        open_accessibility_settings_page();
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
}

#[cfg(target_os = "windows")]
mod windows_impl {
    //! Windows 焦点模型说明（与 macOS 的行为差异，见 RELEASING.md Windows 章节）：
    //! - macOS 面板是 non-activating NSPanel，从不抢焦点；
    //! - Windows 无等价机制（WS_EX_NOACTIVATE 窗口收不到键盘），面板显示时正常激活、
    //!   粘贴前用 SetForegroundWindow 把目标应用拉回前台（hide_and_paste 已有该逻辑）。
    //! - SendInput 无需任何权限（macOS 的 CGEventPost 需要辅助功能授权）。

    use std::sync::atomic::{AtomicUsize, Ordering};

    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
        VIRTUAL_KEY, VK_CONTROL,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, EnumWindows, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId,
        IsWindowVisible, SetForegroundWindow,
    };

    const VK_V_CODE: u16 = 0x56; // 'V'

    /// SendInput 模拟 Ctrl+V（无需辅助功能权限，等价 macOS paste_cmd_v）
    pub(crate) fn paste_cmd_v() {
        fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: vk,
                        wScan: 0,
                        dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            }
        }
        let seq = [
            key(VK_CONTROL, false),
            key(VIRTUAL_KEY(VK_V_CODE), false),
            key(VIRTUAL_KEY(VK_V_CODE), true),
            key(VK_CONTROL, true),
        ];
        unsafe {
            SendInput(&seq, std::mem::size_of::<INPUT>() as i32);
        }
    }

    // ---------- Windows 无辅助功能权限概念：ax_* 恒为 true ----------

    pub(crate) fn ax_trusted(_prompt: bool) -> bool {
        true
    }

    pub(crate) fn ensure_ax() -> bool {
        true
    }

    pub(crate) fn open_accessibility_settings_page() {}

    /// 当前前台窗口所属进程 pid
    pub(crate) fn frontmost_pid() -> i32 {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.0.is_null() {
                return 0;
            }
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));
            pid as i32
        }
    }

    /// EnumWindows 回调间传递候选 hwnd（取该 pid 第一个可见顶层窗口）
    static FOUND_HWND: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let want_pid = lparam.0 as u32;
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));
        if pid == want_pid && IsWindowVisible(hwnd).as_bool() {
            FOUND_HWND.store(hwnd.0 as usize, Ordering::SeqCst);
            return BOOL(0); // 找到即停止枚举
        }
        BOOL(1)
    }

    /// 把指定 pid 的应用拉回前台（找主窗口 → SetForegroundWindow + BringWindowToTop）。
    /// 前台权限：面板刚隐藏时本进程仍是前台进程，此时把前台让给目标是允许的。
    pub(crate) fn activate_app(pid: i32) -> bool {
        FOUND_HWND.store(0, Ordering::SeqCst);
        unsafe {
            let _ = EnumWindows(Some(enum_proc), LPARAM(pid as isize));
            let addr = FOUND_HWND.load(Ordering::SeqCst);
            if addr == 0 {
                return false;
            }
            let hwnd = HWND(addr as *mut core::ffi::c_void);
            let ok = SetForegroundWindow(hwnd);
            let _ = BringWindowToTop(hwnd);
            ok.as_bool()
        }
    }

    /// 供 windows 面板实现复用：判断点 (x,y)（物理像素）是否在指定窗口矩形内
    pub(crate) fn point_in_window(hwnd_addr: usize, x: f64, y: f64) -> bool {
        let hwnd = HWND(hwnd_addr as *mut core::ffi::c_void);
        let mut rect = windows::Win32::Foundation::RECT::default();
        unsafe {
            if !GetWindowRect(hwnd, &mut rect).as_bool() {
                return false;
            }
        }
        x >= rect.left as f64
            && x <= rect.right as f64
            && y >= rect.top as f64
            && y <= rect.bottom as f64
    }
}

#[cfg(target_os = "macos")]
pub(crate) use macos_impl::*;
#[cfg(target_os = "windows")]
pub(crate) use windows_impl::*;
