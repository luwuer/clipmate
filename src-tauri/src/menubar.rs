//! macOS 菜单栏（NSStatusItem）—— 显示面板 + 退出入口
//!
//! 实现要点（与现有 main.rs 风格一致：runtime + msg_send）：
//! - 动态创建 NSObject 子类 ClipMateMenuTarget，addMethod `clicked:`
//! - 所有 NSMenuItem 共享同一个 target 实例，按 tag 区分（1=show, 2=quit）
//! - 回调通过全局 AtomicPtr<AppHandle> 取主线程 AppHandle 直接调用
//!
//! 避免 objc2 block/callback 生命周期坑（第五轮/第九轮教训），改用经典 objc4
//! class_addMethod —— 动态类一旦注册即永久存活，无释放问题。

use std::os::raw::c_char;
use std::sync::atomic::{AtomicPtr, Ordering};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::NSString;

use tauri::{AppHandle, Emitter, Manager};

extern "C" {
    fn objc_allocateClassPair(
        superclass: *const AnyObject,
        name: *const c_char,
        extra_bytes: usize,
    ) -> *mut AnyObject;
    fn objc_registerClassPair(cls: *const AnyObject);
    fn class_addMethod(
        cls: *const AnyObject,
        sel: *const std::ffi::c_void,
        imp: *const std::ffi::c_void,
        types: *const c_char,
    ) -> *mut std::ffi::c_void;
    fn sel_registerName(name: *const c_char) -> *const std::ffi::c_void;
}

/// AppHandle 全局指针，setup 时填入；菜单点击回调（主线程）从这里取
static APP_HANDLE: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());
static MENU_TARGET_CLS: AtomicPtr<AnyObject> = AtomicPtr::new(std::ptr::null_mut());

/// 动态创建 NSObject 子类 + 注册 clicked: 方法；只执行一次
unsafe fn ensure_menu_target_class() -> *const AnyObject {
    let p = MENU_TARGET_CLS.load(Ordering::Acquire);
    if !p.is_null() {
        return p;
    }
    unsafe {
        let super_cls =
            objc2::runtime::AnyClass::get(c"NSObject").unwrap() as *const _ as *const AnyObject;
        let new_cls = objc_allocateClassPair(
            super_cls,
            c"ClipMateMenuTarget".as_ptr() as *const c_char,
            0,
        );
        if new_cls.is_null() {
            eprintln!("[clipmate] objc_allocateClassPair failed");
            return std::ptr::null();
        }
        let sel = sel_registerName(c"clicked:".as_ptr() as *const c_char);
        let types: &[u8] = b"v@:@\0";
        class_addMethod(
            new_cls as *const AnyObject,
            sel,
            target_clicked as *const () as *const std::ffi::c_void,
            types.as_ptr() as *const c_char,
        );
        objc_registerClassPair(new_cls as *const AnyObject);
        MENU_TARGET_CLS.store(new_cls, Ordering::Release);
        new_cls
    }
}

/// 所有 menu item 的回调；通过 tag 分发
unsafe extern "C" fn target_clicked(
    _self: *mut AnyObject,
    _cmd: *const std::ffi::c_void,
    sender: *mut AnyObject,
) {
    let _ = _self;
    let _ = _cmd;
    unsafe {
        if sender.is_null() {
            return;
        }
        let tag: isize = objc2::msg_send![sender, tag];
        let app_ptr = APP_HANDLE.load(Ordering::Acquire);
        if app_ptr.is_null() {
            return;
        }
        let app: &AppHandle = &*(app_ptr as *const AppHandle);
        if tag == 1 {
            // 显示/隐藏面板（visible 时切换隐藏，hidden 时显示）
            crate::toggle_panel_pub(app);
        } else if tag == 2 {
            app.exit(0);
        } else if tag == 3 {
            // 触发系统授权请求 + 打开设置页（避免横幅的情况下提供入口）
            crate::paste::ax_trusted(true);
            crate::paste::open_accessibility_settings_page();
        } else if tag == 4 {
            // 切换主题：读当前 → 取反 → 写 settings.json → 通知前端
            if let Ok(dir) = app.path().app_data_dir() {
                let cur = crate::read_theme_from_settings(&dir);
                let next = if cur == "dark" { "light" } else { "dark" };
                crate::write_theme_to_settings(&dir, next);
                let _ = app.emit("theme-changed", next);
                eprintln!("[clipmate] theme switched to {next}");
            }
        }
    }
}

unsafe fn nsstring(s: &str) -> *mut AnyObject {
    // Retained 泄漏给 Objective-C 持有（一次性常量字符串，数量固定）
    let s: Retained<NSString> = NSString::from_str(s);
    Retained::into_raw(s) as *mut AnyObject
}

/// 安装菜单栏图标 + 菜单（显示面板 / 退出）
/// AppHandle 按 Box 泄漏存全局指针——回调生命周期 = 进程生命周期
pub fn install(app: AppHandle) {
    eprintln!("[clipmate] menubar: install start");
    unsafe {
        // 存 AppHandle 指针供回调使用（into_raw 泄漏，避免悬垂）
        let app_ptr: *mut AppHandle = Box::into_raw(Box::new(app));
        APP_HANDLE.store(app_ptr as *mut (), Ordering::Release);
        eprintln!("[clipmate] menubar: step 1 app handle stored");

        let cls = ensure_menu_target_class();
        eprintln!("[clipmate] menubar: step 2 class={cls:p}");
        if cls.is_null() {
            return;
        }
        // 共享 target 实例
        let target: *mut AnyObject = objc2::msg_send![cls, new];
        eprintln!("[clipmate] menubar: step 3 target={target:p}");
        if target.is_null() {
            return;
        }

        // NSStatusItem
        let sb_cls = objc2::runtime::AnyClass::get(c"NSStatusBar").unwrap();
        let system_bar: *mut AnyObject = objc2::msg_send![sb_cls, systemStatusBar];
        eprintln!("[clipmate] menubar: step 4 systemBar={system_bar:p}");
        let item: *mut AnyObject =
            objc2::msg_send![system_bar, statusItemWithLength: -1.0f64];
        eprintln!("[clipmate] menubar: step 5 item created");

        // 图标：直接用 emoji title（避免 NSImage initWithContentsOfFile 在某些环境下崩溃）
        eprintln!("[clipmate] menubar: step 5.5 creating title");
        let title = nsstring("CM"); // 菜单栏文字标识（emoji 在小尺寸下不易辨认）
        eprintln!("[clipmate] menubar: step 6 title ptr={title:p}");
        let _: () = objc2::msg_send![item, setTitle: title];
        eprintln!("[clipmate] menubar: step 7 title set");
        let _: () = objc2::msg_send![item, setHighlightMode: true];
        eprintln!("[clipmate] menubar: step 8 status item configured");

        // NSMenu
        let menu_cls = objc2::runtime::AnyClass::get(c"NSMenu").unwrap();
        let menu: *mut AnyObject = objc2::msg_send![menu_cls, alloc];
        let menu: *mut AnyObject = objc2::msg_send![menu, init];
        let _: () = objc2::msg_send![menu, setAutoenablesItems: false];

        // 菜单项 1: 显示面板（NSMenuItem 的 alloc 必须发 NSMenuItem 类，不能复用 NSMenu 类）
        let clicked_sel = sel_registerName(c"clicked:".as_ptr() as *const c_char);
        let item_cls = objc2::runtime::AnyClass::get(c"NSMenuItem").unwrap();
        let show_title = nsstring("显示/隐藏剪贴板面板");
        let show_item: *mut AnyObject = objc2::msg_send![item_cls, alloc];
        let show_item: *mut AnyObject = objc2::msg_send![
            show_item,
            initWithTitle: show_title,
            action: clicked_sel,
            keyEquivalent: nsstring("")
        ];
        let _: () = objc2::msg_send![show_item, setTarget: target];
        let _: () = objc2::msg_send![show_item, setTag: 1isize];
        let _: () = objc2::msg_send![show_item, setEnabled: true];
        let _: () = objc2::msg_send![menu, addItem: show_item];

        // 分隔
        let sep: *mut AnyObject = objc2::msg_send![item_cls, separatorItem];
        let _: () = objc2::msg_send![menu, addItem: sep];

        // 菜单项: 重新申请辅助功能权限（避免横幅的情况下提供入口）
        let ax_title = nsstring("申请辅助权限并打开设置");
        let ax_item: *mut AnyObject = objc2::msg_send![item_cls, alloc];
        let ax_item: *mut AnyObject = objc2::msg_send![
            ax_item,
            initWithTitle: ax_title,
            action: clicked_sel,
            keyEquivalent: nsstring("")
        ];
        let _: () = objc2::msg_send![ax_item, setTarget: target];
        let _: () = objc2::msg_send![ax_item, setTag: 3isize];
        let _: () = objc2::msg_send![ax_item, setEnabled: true];
        let _: () = objc2::msg_send![menu, addItem: ax_item];

        // 菜单项: 切换主题（dark ↔ light，写 settings.json + emit 前端）
        let theme_title = nsstring("切换主题");
        let theme_item: *mut AnyObject = objc2::msg_send![item_cls, alloc];
        let theme_item: *mut AnyObject = objc2::msg_send![
            theme_item,
            initWithTitle: theme_title,
            action: clicked_sel,
            keyEquivalent: nsstring("")
        ];
        let _: () = objc2::msg_send![theme_item, setTarget: target];
        let _: () = objc2::msg_send![theme_item, setTag: 4isize];
        let _: () = objc2::msg_send![theme_item, setEnabled: true];
        let _: () = objc2::msg_send![menu, addItem: theme_item];

        // 菜单项 2: 退出
        let quit_title = nsstring("退出 ClipMate");
        let quit_item: *mut AnyObject = objc2::msg_send![item_cls, alloc];
        let quit_item: *mut AnyObject = objc2::msg_send![
            quit_item,
            initWithTitle: quit_title,
            action: clicked_sel,
            keyEquivalent: nsstring("")
        ];
        let _: () = objc2::msg_send![quit_item, setTarget: target];
        let _: () = objc2::msg_send![quit_item, setTag: 2isize];
        let _: () = objc2::msg_send![quit_item, setEnabled: true];
        let _: () = objc2::msg_send![menu, addItem: quit_item];

        // 关联菜单 → 状态项
        let _: () = objc2::msg_send![item, setMenu: menu];

        eprintln!("[clipmate] menubar installed");
    }
}
