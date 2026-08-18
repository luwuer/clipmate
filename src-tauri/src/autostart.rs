//! R20: 开机自启（macOS Login Item，LaunchAgent plist 方案）
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
//!
//! 路径来源：std::env::current_exe() —— 用户移动 .app 后下次启动路径自动匹配新位置；
//! setup 时调用 sync_path_if_stale() 检测 plist 里的路径与当前不一致就重写。

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
