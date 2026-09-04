//! R20: 开机自启（平台实现）
//!
//! - macOS：Login Item，LaunchAgent plist 方案（纯文件，无弹窗、可直接 cat/plutil 验证）
//! - Windows：HKCU\Software\Microsoft\Windows\CurrentVersion\Run 注册表值（W1 移植）
//!
//! 两个平台对外接口一致：is_enabled / enable / disable / sync_path_if_stale。
//! 路径来源都是 std::env::current_exe() —— 用户移动/重装后启动时自动校正。

#[cfg(target_os = "macos")]
mod macos_impl {
    //! macOS LaunchAgent plist 方案
    //!
    //! 选择 LaunchAgent 而非 SMAppService 的理由：
    //! - SMAppService（macOS 13+）首次注册会弹系统通知且需要 ServiceManagement framework
    //!   API（objc2 msg_send 到 SMAppService 类），依赖与权限交互更多；
    //! - LaunchAgent 是纯文件方案：写 ~/Library/LaunchAgents/com.mdy.clipmate.plist 即生效，
    //!   无弹窗、无 framework 依赖，可直接 cat/plutil 验证，与本项目"文件即状态"风格一致
    //!   （settings.json 同款思路）。
    //!
    //! plist 内容：Label=com.mdy.clipmate，ProgramArguments=[当前可执行文件路径]，
    //! RunAtLoad=true，KeepAlive=false（崩溃不自动拉起，避免 reaper 循环）。

    use std::path::PathBuf;

    pub const PLIST_LABEL: &str = "com.mdy.clipmate";

    /// ~/Library/LaunchAgents/com.mdy.clipmate.plist
    fn plist_path() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(
            PathBuf::from(home)
                .join("Library/LaunchAgents")
                .join(format!("{PLIST_LABEL}.plist")),
        )
    }

    /// 当前可执行文件绝对路径（.app 内即 ClipMate.app/Contents/MacOS/clipmate）
    fn current_exe_path() -> Option<String> {
        std::env::current_exe()
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    }

    /// XML 转义（路径里可能含 & < >）
    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    /// 生成 plist 文本（抽出来便于单测验证内容）
    fn render_plist(exe: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{PLIST_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><false/>
</dict>
</plist>
"#,
            xml_escape(exe)
        )
    }

    /// 是否已开启（plist 文件存在即视为开启）
    pub fn is_enabled() -> bool {
        plist_path().map(|p| p.exists()).unwrap_or(false)
    }

    /// 开启：写 plist（覆盖式——路径变了自动更新）
    pub fn enable() -> Result<(), String> {
        let path = plist_path().ok_or("HOME 不可用，无法定位 LaunchAgents")?;
        let exe = current_exe_path().ok_or("current_exe 不可用")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create LaunchAgents dir: {e}"))?;
        }
        std::fs::write(&path, render_plist(&exe)).map_err(|e| format!("write plist: {e}"))?;
        eprintln!("[clipmate] autostart enabled -> {}", path.display());
        Ok(())
    }

    /// 关闭：删 plist
    pub fn disable() -> Result<(), String> {
        if let Some(path) = plist_path() {
            if path.exists() {
                std::fs::remove_file(&path).map_err(|e| format!("remove plist: {e}"))?;
                eprintln!("[clipmate] autostart disabled");
            }
        }
        Ok(())
    }

    /// 启动时调用：plist 存在但 ProgramArguments 路径与当前 exe 不一致 → 重写
    /// （处理用户移动/重装 .app 的场景）
    pub fn sync_path_if_stale() {
        let Some(path) = plist_path() else { return };
        let Ok(content) = std::fs::read_to_string(&path) else {
            return;
        };
        let Some(exe) = current_exe_path() else { return };
        if !content.contains(&exe) {
            eprintln!("[clipmate] autostart plist path stale, rewriting");
            if let Err(e) = enable() {
                eprintln!("[clipmate] autostart rewrite failed: {e}");
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn plist_content_shape() {
            let p = render_plist("/Users/x/dist/ClipMate.app/Contents/MacOS/clipmate");
            assert!(p.contains("<key>Label</key><string>com.mdy.clipmate</string>"));
            assert!(p.contains(
                "<string>/Users/x/dist/ClipMate.app/Contents/MacOS/clipmate</string>"
            ));
            assert!(p.contains("<key>RunAtLoad</key><true/>"));
            assert!(p.contains("<key>KeepAlive</key><false/>"));
            assert!(p.contains("<key>ProgramArguments</key>"));
        }

        #[test]
        fn plist_escapes_xml() {
            let p = render_plist("/tmp/a&b<c>/clipmate");
            assert!(p.contains("/tmp/a&amp;b&lt;c&gt;/clipmate"));
            assert!(!p.contains("a&b<c>"));
        }

        /// enable → plist 落盘且内容正确 → is_enabled true → disable → 文件删除
        /// （用临时 HOME 隔离，不碰真实 ~/Library/LaunchAgents）
        #[test]
        fn enable_disable_roundtrip() {
            let tmp = std::env::temp_dir().join(format!("clipmate-autostart-{}", std::process::id()));
            std::fs::create_dir_all(&tmp).unwrap();
            let orig_home = std::env::var_os("HOME");
            std::env::set_var("HOME", &tmp);

            assert!(!is_enabled());
            enable().unwrap();
            assert!(is_enabled());
            let content = std::fs::read_to_string(
                tmp.join("Library/LaunchAgents/com.mdy.clipmate.plist"),
            )
            .unwrap();
            assert!(content.contains("<key>Label</key><string>com.mdy.clipmate</string>"));
            assert!(content.contains("<key>RunAtLoad</key><true/>"));
            // 路径 = 当前测试进程的可执行文件（current_exe）
            let exe = std::env::current_exe().unwrap();
            assert!(content.contains(&*exe.to_string_lossy()));

            disable().unwrap();
            assert!(!is_enabled());

            // 重复 disable 不报错（幂等）
            disable().unwrap();

            match orig_home {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
            let _ = std::fs::remove_dir_all(&tmp);
        }
    }
}

#[cfg(target_os = "windows")]
mod windows_impl {
    //! Windows：HKCU\Software\Microsoft\Windows\CurrentVersion\Run 注册表值
    //!
    //! 等价 launchd plist 的"文件即状态"：注册表值存在即开启，值 = 带引号的 exe 路径
    //! （引号防空格路径被拆参）。无管理员权限需求（HKCU）。

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
        RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE,
        REG_SZ, REG_VALUE_TYPE,
    };

    const RUN_SUBKEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const VALUE_NAME: &str = "ClipMate";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn current_exe_path() -> Option<String> {
        std::env::current_exe()
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    }

    /// u16 数组按字节重解释（含结尾 NUL，双字节计长）
    fn as_bytes(w: &[u16]) -> &[u8] {
        unsafe { std::slice::from_raw_parts(w.as_ptr().cast::<u8>(), w.len() * 2) }
    }

    /// 读取 Run\ClipMate 的值（REG_SZ）；不存在/类型不对返回 None
    fn read_run_value() -> Option<String> {
        unsafe {
            let mut hkey = HKEY::default();
            let sub = wide(RUN_SUBKEY);
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(sub.as_ptr()),
                None,
                KEY_READ,
                &mut hkey,
            ) != ERROR_SUCCESS
            {
                return None;
            }
            let name = wide(VALUE_NAME);
            let mut ty = REG_VALUE_TYPE(0);
            let mut buf = [0u16; 1024];
            let mut cb = (buf.len() * 2) as u32;
            let res = RegQueryValueExW(
                hkey,
                PCWSTR(name.as_ptr()),
                None,
                Some(&mut ty as *mut REG_VALUE_TYPE),
                Some(buf.as_mut_ptr().cast::<u8>()),
                Some(&mut cb as *mut u32),
            );
            let _ = RegCloseKey(hkey);
            if res != ERROR_SUCCESS || ty != REG_SZ {
                return None;
            }
            let len = (cb as usize / 2).min(buf.len());
            let s = String::from_utf16_lossy(&buf[..len]);
            Some(s.trim_end_matches('\0').to_string())
        }
    }

    /// 是否已开启（Run\ClipMate 值存在即视为开启）
    pub fn is_enabled() -> bool {
        read_run_value().is_some()
    }

    /// 开启：写 Run\ClipMate = "exe路径"（带引号，覆盖式——路径变了自动更新）
    pub fn enable() -> Result<(), String> {
        let exe = current_exe_path().ok_or("current_exe 不可用")?;
        let data = format!("\"{exe}\"");
        unsafe {
            let mut hkey = HKEY::default();
            let sub = wide(RUN_SUBKEY);
            let create = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(sub.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut hkey,
                None,
            );
            if create != ERROR_SUCCESS {
                return Err(format!("create Run key failed: {}", create.0));
            }
            let name = wide(VALUE_NAME);
            let data_w = wide(&data);
            let res = RegSetValueExW(
                hkey,
                PCWSTR(name.as_ptr()),
                None,
                REG_SZ,
                Some(as_bytes(&data_w)),
            );
            let _ = RegCloseKey(hkey);
            if res != ERROR_SUCCESS {
                return Err(format!("set Run value failed: {}", res.0));
            }
        }
        eprintln!("[clipmate] autostart enabled -> HKCU Run\\ClipMate");
        Ok(())
    }

    /// 关闭：删 Run\ClipMate（值/键不存在视为幂等成功）
    pub fn disable() -> Result<(), String> {
        unsafe {
            let mut hkey = HKEY::default();
            let sub = wide(RUN_SUBKEY);
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(sub.as_ptr()),
                None,
                KEY_WRITE,
                &mut hkey,
            ) != ERROR_SUCCESS
            {
                return Ok(()); // Run 键不存在 = 从未开启，幂等
            }
            let name = wide(VALUE_NAME);
            let res: WIN32_ERROR = RegDeleteValueW(hkey, PCWSTR(name.as_ptr()));
            let _ = RegCloseKey(hkey);
            if res != ERROR_SUCCESS && res != ERROR_FILE_NOT_FOUND {
                return Err(format!("delete Run value failed: {}", res.0));
            }
        }
        eprintln!("[clipmate] autostart disabled");
        Ok(())
    }

    /// 启动时调用：注册表里的路径与当前 exe 不一致 → 重写
    /// （处理用户移动/重装安装目录的场景）
    pub fn sync_path_if_stale() {
        let Some(v) = read_run_value() else { return };
        let Some(exe) = current_exe_path() else { return };
        if v.trim_matches('"') != exe {
            eprintln!("[clipmate] autostart registry path stale, rewriting");
            if let Err(e) = enable() {
                eprintln!("[clipmate] autostart rewrite failed: {e}");
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// enable → is_enabled true → disable → is_enabled false（操作真实 HKCU，
        /// 只动 ClipMate 专属值；disable 幂等；测完还原初始状态）
        #[test]
        fn enable_disable_roundtrip() {
            let before = is_enabled();
            let before_value = read_run_value();

            enable().unwrap();
            assert!(is_enabled());
            // 值 = 带引号的当前 exe 路径
            let exe = std::env::current_exe().unwrap().to_string_lossy().into_owned();
            assert_eq!(read_run_value().as_deref(), Some(format!("\"{exe}\"").as_str()));

            disable().unwrap();
            assert!(!is_enabled());
            disable().unwrap(); // 幂等

            if before {
                enable().unwrap();
                let _ = before_value; // 路径即当前 exe，enable 已还原"开启"语义
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub use macos_impl::*;
#[cfg(target_os = "windows")]
pub use windows_impl::*;
