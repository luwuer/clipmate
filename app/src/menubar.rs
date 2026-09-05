//! 菜单栏/托盘入口 —— 显示面板 + 退出 + 设置项
//!
//! 平台实现（W1 Windows 移植）：
//! - macOS：NSStatusItem（runtime + msg_send，与 main.rs 风格一致）
//! - Windows：Tauri 2 跨平台 TrayIcon API（系统托盘）
//! 菜单项语义两平台一致：显示面板 / 主题 / 开机自启 / 面板位置 / 退出。

#[cfg(target_os = "macos")]
mod macos_impl {
    //! macOS 菜单栏（NSStatusItem）
    //!
    //! 实现要点（与现有 main.rs 风格一致：runtime + msg_send）：
    //! - 动态创建 NSObject 子类 ClipMateMenuTarget，addMethod `clicked:`
    //! - 所有 NSMenuItem 共享同一个 target 实例，按 tag 区分
    //!   （1=show, 2=quit, 3=辅助权限, 4=主题, 5=开机自启, 6=面板位置,
    //!    7=设置背景图, 8=清除背景图）
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
        ) -> *const std::ffi::c_void;
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
            } else if tag == 5 {
                // R20: 开机自启开关（LaunchAgent plist）——勾选态直接画在本菜单项上
                let enabled = crate::autostart::is_enabled();
                let result = if enabled {
                    crate::autostart::disable()
                } else {
                    crate::autostart::enable()
                };
                match result {
                    Ok(()) => {
                        let new_state: isize = if enabled { 0 } else { 1 }; // NSOffState/NSOnState
                        let _: () = objc2::msg_send![sender, setState: new_state];
                        eprintln!("[clipmate] autostart toggled -> {}", !enabled);
                    }
                    Err(e) => eprintln!("[clipmate] autostart toggle failed: {e}"),
                }
            } else if tag == 4 {
                // 切换主题：读当前 → 取反 → 写 settings.json → 通知前端
                if let Ok(dir) = app.path().app_data_dir() {
                    let cur = crate::read_theme_from_settings(&dir);
                    let next = if cur == "dark" { "light" } else { "dark" };
                    crate::write_theme_to_settings(&dir, next);
                    let _ = app.emit("theme-changed", next);
                    eprintln!("[clipmate] theme switched to {next}");
                }
            } else if tag == 6 {
                // 切换面板位置模式：fixed ↔ cursor（光标所在屏幕顶部居中 ↔ 贴光标）
                if let Ok(dir) = app.path().app_data_dir() {
                    let cur = crate::read_panel_position_from_settings(&dir);
                    let next = if cur == "fixed" { "cursor" } else { "fixed" };
                    crate::write_panel_position_to_settings(&dir, next);
                    // 同步勾选态与标题
                    let _: () = objc2::msg_send![sender, setState: 1isize];
                    eprintln!("[clipmate] panel_position switched to {next}");
                }
            } else if tag == 7 {
                // 设置自定义背景图：跨平台文件选择对话框（tauri-plugin-dialog，非阻塞）
                use tauri_plugin_dialog::DialogExt;
                let app2 = app.clone();
                app.dialog()
                    .file()
                    .add_filter("图片", &["png", "jpg", "jpeg", "gif", "webp", "bmp", "heic"])
                    .pick_file(move |file| {
                        let Some(path) = file.and_then(|f| f.into_path().ok()) else {
                            return;
                        };
                        if let Ok(dir) = app2.path().app_data_dir() {
                            crate::write_background_to_settings(
                                &dir,
                                Some(&path.to_string_lossy()),
                            );
                            let _ = app2.emit("background-changed", ());
                            eprintln!("[clipmate] background set: {}", path.display());
                        }
                    });
            } else if tag == 8 {
                // 清除自定义背景图（移除字段 + 通知前端恢复纯色）
                if let Ok(dir) = app.path().app_data_dir() {
                    crate::write_background_to_settings(&dir, None);
                    let _ = app.emit("background-changed", ());
                    eprintln!("[clipmate] background cleared");
                }
            } else if tag >= 100 && tag <= 107 {
                // 预设背景：tag 100..107 对应 preset:1..8
                let id: u32 = (tag - 99) as u32;
                if let Ok(dir) = app.path().app_data_dir() {
                    crate::write_background_to_settings(&dir, Some(&format!("preset:{id}")));
                    let _ = app.emit("background-changed", ());
                    eprintln!("[clipmate] background preset:{id}");
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
            let app_ptr: *mut AppHandle = Box::into_raw(Box::new(app.clone()));
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

            // 图标：SF Symbol 剪贴板符号（系统符号，无文件加载崩溃风险，模板渲染自动适配深浅菜单栏）
            // 加载失败时回退 "CM" 文字标识
            let symbol_name = nsstring("doc.on.clipboard");
            let nsimage_cls = objc2::runtime::AnyClass::get(c"NSImage").unwrap();
            let image: *mut AnyObject = objc2::msg_send![
                nsimage_cls,
                imageWithSystemSymbolName: symbol_name
                accessibilityDescription: std::ptr::null_mut::<AnyObject>()
            ];
            eprintln!("[clipmate] menubar: step 5.5 symbol image ptr={image:p}");
            if !image.is_null() {
                let button: *mut AnyObject = objc2::msg_send![item, button];
                eprintln!("[clipmate] menubar: step 5.6 button ptr={button:p}");
                if !button.is_null() {
                    let _: () = objc2::msg_send![button, setImage: image];
                    eprintln!("[clipmate] menubar: step 7 SF Symbol icon set");
                }
            } else {
                let title = nsstring("CM"); // 回退：文字标识
                let _: () = objc2::msg_send![item, setTitle: title];
                eprintln!("[clipmate] menubar: step 7 fallback title 'CM' set");
            }
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

            // 菜单项: 开机自启（勾选态：初始读 plist 是否存在；点击切换写/删 plist）
            let autostart_title = nsstring("开机自启");
            let autostart_item: *mut AnyObject = objc2::msg_send![item_cls, alloc];
            let autostart_item: *mut AnyObject = objc2::msg_send![
                autostart_item,
                initWithTitle: autostart_title,
                action: clicked_sel,
                keyEquivalent: nsstring("")
            ];
            let _: () = objc2::msg_send![autostart_item, setTarget: target];
            let _: () = objc2::msg_send![autostart_item, setTag: 5isize];
            let _: () = objc2::msg_send![autostart_item, setEnabled: true];
            let initial_state: isize = if crate::autostart::is_enabled() { 1 } else { 0 };
            let _: () = objc2::msg_send![autostart_item, setState: initial_state];
            let _: () = objc2::msg_send![menu, addItem: autostart_item];

            // 菜单项: 切换面板位置模式（fixed ↔ cursor）——固定位置适合全局唤起，
            // 光标跟随适合上下文紧贴输入位置
            let position_title = nsstring("面板位置：固定 / 跟随光标");
            let position_item: *mut AnyObject = objc2::msg_send![item_cls, alloc];
            let position_item: *mut AnyObject = objc2::msg_send![
                position_item,
                initWithTitle: position_title,
                action: clicked_sel,
                keyEquivalent: nsstring("")
            ];
            let _: () = objc2::msg_send![position_item, setTarget: target];
            let _: () = objc2::msg_send![position_item, setTag: 6isize];
            let _: () = objc2::msg_send![position_item, setEnabled: true];
            // 初始勾选态反映当前 settings（默认 fixed）
            if let Ok(dir) = app.path().app_data_dir() {
                let cur = crate::read_panel_position_from_settings(&dir);
                if cur == "fixed" {
                    let _: () = objc2::msg_send![position_item, setState: 1isize];
                }
            }
            let _: () = objc2::msg_send![menu, addItem: position_item];

            // ---- 子菜单「背景」：8 个内置预设 + 自定义图片… + 清除背景 ----
            // NSMenu submenu：alloc/init 一个独立菜单，addItem 把子项挂进去；
            // 再创建带 title 的父 NSMenuItem，setSubmenu: 关联。
            let bg_submenu: *mut AnyObject = objc2::msg_send![menu_cls, alloc];
            let bg_submenu: *mut AnyObject = objc2::msg_send![bg_submenu, init];
            let _: () = objc2::msg_send![bg_submenu, setAutoenablesItems: false];

            // 预设 1..8（tag 100+i → preset:i+1）
            let preset_titles = [
                "1 · 黄昏天台",
                "2 · 赛博夜都",
                "3 · 和风庭院",
                "4 · 星空原野",
                "5 · 夏日海边",
                "6 · 雪夜小镇",
                "7 · 向日葵花田",
                "8 · 雨夜街灯",
            ];
            for (i, title) in preset_titles.iter().enumerate() {
                let tag = 100 + i as isize;
                let p_title = nsstring(title);
                let p_item: *mut AnyObject = objc2::msg_send![item_cls, alloc];
                let p_item: *mut AnyObject = objc2::msg_send![
                    p_item,
                    initWithTitle: p_title,
                    action: clicked_sel,
                    keyEquivalent: nsstring("")
                ];
                let _: () = objc2::msg_send![p_item, setTarget: target];
                let _: () = objc2::msg_send![p_item, setTag: tag];
                let _: () = objc2::msg_send![p_item, setEnabled: true];
                // 勾选态反映当前预设
                if let Ok(dir) = app.path().app_data_dir() {
                    let cur = crate::read_background_from_settings(&dir)
                        .unwrap_or_default();
                    if cur == format!("preset:{}", i + 1) {
                        let _: () = objc2::msg_send![p_item, setState: 1isize];
                    }
                }
                let _: () = objc2::msg_send![bg_submenu, addItem: p_item];
            }

            // 子菜单分隔
            let bg_sep: *mut AnyObject = objc2::msg_send![item_cls, separatorItem];
            let _: () = objc2::msg_send![bg_submenu, addItem: bg_sep];

            // 自定义图片…（tag 7）
            let setbg_title = nsstring("自定义图片…");
            let setbg_item: *mut AnyObject = objc2::msg_send![item_cls, alloc];
            let setbg_item: *mut AnyObject = objc2::msg_send![
                setbg_item,
                initWithTitle: setbg_title,
                action: clicked_sel,
                keyEquivalent: nsstring("")
            ];
            let _: () = objc2::msg_send![setbg_item, setTarget: target];
            let _: () = objc2::msg_send![setbg_item, setTag: 7isize];
            let _: () = objc2::msg_send![setbg_item, setEnabled: true];
            let _: () = objc2::msg_send![bg_submenu, addItem: setbg_item];

            // 清除背景（tag 8）
            let clearbg_title = nsstring("清除背景");
            let clearbg_item: *mut AnyObject = objc2::msg_send![item_cls, alloc];
            let clearbg_item: *mut AnyObject = objc2::msg_send![
                clearbg_item,
                initWithTitle: clearbg_title,
                action: clicked_sel,
                keyEquivalent: nsstring("")
            ];
            let _: () = objc2::msg_send![clearbg_item, setTarget: target];
            let _: () = objc2::msg_send![clearbg_item, setTag: 8isize];
            let _: () = objc2::msg_send![clearbg_item, setEnabled: true];
            let _: () = objc2::msg_send![bg_submenu, addItem: clearbg_item];

            // 父项「背景」承载 submenu（tag=99 在 target_clicked 分发器里不匹配 → 无副作用）
            let bg_parent_title = nsstring("背景");
            let bg_parent: *mut AnyObject = objc2::msg_send![item_cls, alloc];
            let bg_parent: *mut AnyObject = objc2::msg_send![
                bg_parent,
                initWithTitle: bg_parent_title,
                action: clicked_sel,
                keyEquivalent: nsstring("")
            ];
            let _: () = objc2::msg_send![bg_parent, setTarget: target];
            let _: () = objc2::msg_send![bg_parent, setTag: 99isize];
            let _: () = objc2::msg_send![bg_parent, setSubmenu: bg_submenu];
            let _: () = objc2::msg_send![menu, addItem: bg_parent];

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
}

#[cfg(target_os = "windows")]
mod windows_impl {
    //! Windows 系统托盘（Tauri 2 TrayIcon API，等价 macOS NSStatusItem）
    //!
    //! 与 macOS 菜单项语义一一对应；「申请辅助权限」省略——Windows 无此概念
    //! （SendInput 不需要授权）。

    use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::TrayIconBuilder;
    use tauri::{AppHandle, Emitter, Manager};

    pub fn install(app: AppHandle) {
        use tauri::menu::Submenu;

        let show = MenuItem::with_id(&app, "show", "显示/隐藏剪贴板面板", true, None::<&str>);
        let theme = MenuItem::with_id(&app, "theme", "切换主题", true, None::<&str>);
        let autostart = CheckMenuItem::with_id(
            &app,
            "autostart",
            "开机自启",
            true,
            crate::autostart::is_enabled(),
            None::<&str>,
        );
        let position = MenuItem::with_id(
            &app,
            "position",
            "面板位置：固定 / 跟随光标",
            true,
            None::<&str>,
        );

        // 8 个预设菜单项（id = "bg1".."bg8"），先收集 vec 引用再传给 Submenu
        let preset_titles = [
            "1 · 黄昏天台",
            "2 · 赛博夜都",
            "3 · 和风庭院",
            "4 · 星空原野",
            "5 · 夏日海边",
            "6 · 雪夜小镇",
            "7 · 向日葵花田",
            "8 · 雨夜街灯",
        ];
        let preset_items: Vec<Result<MenuItem<tauri::Wry>, _>> = (1..=8)
            .map(|i| {
                let id = format!("bg{i}");
                MenuItem::with_id(
                    &app,
                    &id,
                    preset_titles[i - 1],
                    true,
                    None::<&str>,
                )
            })
            .collect();
        let preset_refs: Vec<&MenuItem<tauri::Wry>> = match preset_items.iter().collect::<Result<Vec<_>, _>>() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[clipmate] tray preset menu item create failed: {e}");
                return;
            }
        };
        let setbg = MenuItem::with_id(&app, "setbg", "自定义图片…", true, None::<&str>);
        let clearbg = MenuItem::with_id(&app, "clearbg", "清除背景", true, None::<&str>);

        let (show, theme, autostart, position, setbg, clearbg) = match (
            show,
            theme,
            autostart,
            position,
            setbg,
            clearbg,
        ) {
            (Ok(a), Ok(b), Ok(c), Ok(d), Ok(e), Ok(f)) => (a, b, c, d, e, f),
            (Err(e), ..)
            | (_, Err(e), ..)
            | (_, _, Err(e), ..)
            | (_, _, _, Err(e), ..)
            | (_, _, _, _, Err(e), ..)
            | (_, _, _, _, _, Err(e)) => {
                eprintln!("[clipmate] tray menu item create failed: {e}");
                return;
            }
        };

        // 背景子菜单：8 预设 + 自定义 + 清除
        let mut bg_kids: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> = Vec::new();
        bg_kids.extend(preset_refs.iter().map(|m| m as &dyn tauri::menu::IsMenuItem<tauri::Wry>));
        bg_kids.push(&setbg);
        bg_kids.push(&clearbg);
        let bg_submenu = Submenu::with_id(&app, "background", "背景", true, &bg_kids);
        let bg_submenu = match bg_submenu {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[clipmate] tray background submenu create failed: {e}");
                return;
            }
        };

        let menu = Menu::with_items(&app, &[&show, &theme, &autostart, &position, &bg_submenu]);
        let menu = match menu {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[clipmate] tray menu create failed: {e}");
                return;
            }
        };
        // 分隔符 + 退出项：separator 创建失败不致命（跳过分隔符继续）
        if let Ok(sep) = PredefinedMenuItem::separator(&app) {
            let _ = menu.append(&sep);
        }
        let quit = MenuItem::with_id(&app, "quit", "退出 ClipMate", true, None::<&str>);
        if let Err(e) = quit {
            eprintln!("[clipmate] tray quit item create failed: {e}");
            return;
        }
        let quit = quit.unwrap();
        if let Err(e) = menu.append(&quit) {
            eprintln!("[clipmate] tray append quit item failed: {e}");
        }

        // 勾选态实时更新需要 CheckMenuItem 句柄（v2 menu 类型 Send+Sync，可进闭包）
        let autostart_item = autostart.clone();

        let mut builder = TrayIconBuilder::with_id("clipmate-tray")
            .tooltip("ClipMate")
            .menu(&menu)
            .show_menu_on_left_click(true)
            .on_menu_event(move |app, event| match event.id().as_ref() {
                "show" => crate::toggle_panel_pub(app),
                "theme" => {
                    if let Ok(dir) = app.path().app_data_dir() {
                        let cur = crate::read_theme_from_settings(&dir);
                        let next = if cur == "dark" { "light" } else { "dark" };
                        crate::write_theme_to_settings(&dir, next);
                        let _ = app.emit("theme-changed", next);
                        eprintln!("[clipmate] theme switched to {next}");
                    }
                }
                "autostart" => {
                    let enabled = crate::autostart::is_enabled();
                    let result = if enabled {
                        crate::autostart::disable()
                    } else {
                        crate::autostart::enable()
                    };
                    match result {
                        Ok(()) => {
                            let _ = autostart_item.set_checked(!enabled);
                            eprintln!("[clipmate] autostart toggled -> {}", !enabled);
                        }
                        Err(e) => eprintln!("[clipmate] autostart toggle failed: {e}"),
                    }
                }
                "position" => {
                    if let Ok(dir) = app.path().app_data_dir() {
                        let cur = crate::read_panel_position_from_settings(&dir);
                        let next = if cur == "fixed" { "cursor" } else { "fixed" };
                        crate::write_panel_position_to_settings(&dir, next);
                        eprintln!("[clipmate] panel_position switched to {next}");
                    }
                }
                "setbg" => {
                    use tauri_plugin_dialog::DialogExt;
                    let app2 = app.clone();
                    app.dialog()
                        .file()
                        .add_filter("图片", &["png", "jpg", "jpeg", "gif", "webp", "bmp"])
                        .pick_file(move |file| {
                            let Some(path) = file.and_then(|f| f.into_path().ok()) else {
                                return;
                            };
                            if let Ok(dir) = app2.path().app_data_dir() {
                                crate::write_background_to_settings(
                                    &dir,
                                    Some(&path.to_string_lossy()),
                                );
                                let _ = app2.emit("background-changed", ());
                                eprintln!("[clipmate] background set: {}", path.display());
                            }
                        });
                }
                "clearbg" => {
                    if let Ok(dir) = app.path().app_data_dir() {
                        crate::write_background_to_settings(&dir, None);
                        let _ = app.emit("background-changed", ());
                        eprintln!("[clipmate] background cleared");
                    }
                }
                id if id.starts_with("bg") && id.len() == 3 => {
                    // 预设：id = "bg1".."bg8"
                    if let Ok(n) = id[2..].parse::<u32>() {
                        if (1..=8).contains(&n) {
                            if let Ok(dir) = app.path().app_data_dir() {
                                crate::write_background_to_settings(&dir, Some(&format!("preset:{n}")));
                                let _ = app.emit("background-changed", ());
                                eprintln!("[clipmate] background preset:{n}");
                            }
                        }
                    }
                }
                "quit" => app.exit(0),
                _ => {}
            });

        if let Some(icon) = app.default_window_icon() {
            builder = builder.icon(icon.clone());
        }

        // TrayIcon 由 app 内部管理（tray_by_id 可再取），forget 防 drop 摘掉图标
        match builder.build(&app) {
            Ok(tray) => std::mem::forget(tray),
            Err(e) => eprintln!("[clipmate] tray icon build failed: {e}"),
        }
        eprintln!("[clipmate] tray installed");
    }
}

#[cfg(target_os = "macos")]
pub use macos_impl::install;
#[cfg(target_os = "windows")]
pub use windows_impl::install;
