# ClipMate

一个极简的 macOS 剪贴板历史工具，灵感来自 CleanClip。

- **F2** 唤起最近复制内容的面板，方向键选择，Enter 粘贴回当前应用，Esc 关闭
- 记录 **文本** 与 **图片**
- 支持 **搜索过滤**
- 点击其他位置自动隐藏面板

## 效果预览

按 F2 弹出的面板（在当前活动窗口上方）：

```
┌──────────────────────────────────┐
│ 搜索剪贴板历史…              ⌫   │  ← 搜索框 + 清空
├──────────────────────────────────┤
│ https://example.com/api/v1/...   │  ← 最新，蓝色高亮
│                              ✕   │
│ 订单号 #2026-0818-0001            │
│ 金额 ¥1,234.56 …                  │
│                              ✕   │
│ hello clipmate 第一条              │
│                              ✕   │
├──────────────────────────────────┤
│  ↑↓ 选择  ⏎ 粘贴  Esc 关闭  F2 切换 │
└──────────────────────────────────┘
```

## 技术栈

- **Tauri 2** + **Rust**：后端逻辑、全局热键、剪贴板监听、Cmd+V 模拟
- **vanilla HTML/CSS/JS**：前端面板，无 Node 构建链，单文件即可运行
- 依赖：arboard（剪贴板读写）、core-graphics（CGEvent）、png、objc2（NSPasteboard changeCount）、tauri-plugin-global-shortcut

## 编译

需要 [Rust 工具链](https://rustup.rs)：

```bash
cd clipmate/src-tauri
cargo build --release
```

## 打包成 .app

```bash
cd clipmate/src-tauri
npx -y @tauri-apps/cli@2 build
# 补上 LSUIElement（无 Dock 图标）和 ad-hoc 签名
/usr/libexec/PlistBuddy -c "Add :LSUIElement bool true" \
  target/release/bundle/macos/ClipMate.app/Contents/Info.plist
codesign -f -s - --deep target/release/bundle/macos/ClipMate.app

# 制作 dmg
cd target/release/bundle
rm -rf dmg_staging && mkdir dmg_staging
cp -R macos/ClipMate.app dmg_staging/
ln -s /Applications dmg_staging/Applications
hdiutil create -volname "ClipMate" -srcfolder dmg_staging -ov -format UDZO ClipMate.dmg
rm -rf dmg_staging
```

产物：
- `clipmate/dist/ClipMate.app` — 双击运行（约 5.6 MB）
- `clipmate/dist/ClipMate.dmg` — 标准安装镜像（约 2.7 MB，含「拖到 Applications」引导）

## 运行

```bash
# 方式 1：双击 ClipMate.app
open dist/ClipMate.app

# 方式 2：命令行启动（后台常驻，F2 唤起）
open dist/ClipMate.app

# 调试用：启动后直接显示面板
open dist/ClipMate.app --args --show
```

启动后菜单栏不会出现图标（应用为 Accessory 策略），不抢焦点。
后台进程一直运行，直到手动退出：

```bash
pkill -f clipmate
```

## 首次使用：授权

粘贴到其他应用需要 macOS **辅助功能** 权限。ClipMate 会在启动时自动请求一次（macOS 系统弹窗），点「打开系统设置」即可。如果在设置里找不到 ClipMate 入口（之前拒过/列表为空），用以下任一方式：

**方式 A（推荐）**：系统设置 → 隐私与安全性 → 辅助功能 → 滚动到列表底部 → 点 **+** → 选择 ClipMate.app（路径 `/Users/mdy/workspace_test/repos/clipmate/dist/ClipMate.app`）→ 勾选

**方式 B**：终端执行后重启 ClipMate，弹窗会自动重新出现

```bash
tccutil reset Accessibility com.mdy.clipmate
```

菜单栏图标 → "打开设置" 按钮也会同时尝试触发系统请求 + 打开两个版本的设置 URL（兼容 macOS 13 与 macOS 15）。
4. 勾选 **ClipMate**
5. 重启 ClipMate 生效

> 如果你希望 ClipMate **开机自启**，把 `clipmate` 复制到 `~/Library/LaunchAgents/` 或用系统设置 → 通用 → 登录项添加。

## 键盘操作

| 按键 | 动作 |
| --- | --- |
| **F2** | 显示/隐藏面板 |
| **↑ / ↓** | 在历史中移动选择 |
| **Enter** | 选中并粘贴到当前应用 |
| **Esc** | 关闭面板 |
| **输入文字** | 实时过滤历史（文本内容） |
| 鼠标悬停 + **✕** | 删除单条记录 |
| 顶部 **⌫** 按钮 | 清空全部历史 |

## 行为细节

- **去重**：连续复制相同内容不会产生重复条目
- **顺序**：选中并粘贴的条目会移到最前（按使用频率排序）
- **过滤**：搜索时输入「图片」「image」「img」「png」可筛选图片
- **限制**：单条文本最大 2 MB，单张图片最大 8 MB，最多保留 300 条
- **不记录**：剪贴板上的「文件」、「空白内容」会跳过
- **不持久化**：历史仅保留在内存中，重启后清空（按设计取舍，保持简单）

## F2 冲突说明

macOS 默认把 F1–F12 用作亮度/音量等系统快捷键，按 F2 可能无反应。
在「系统设置 → 键盘 → 键盘快捷键 → 功能键」中勾选「将 F1、F2 等键用作标准功能键」即可。

或者编辑 `src-tauri/src/main.rs` 顶部的 `HOTKEY` 常量改为其他组合（支持 `Cmd+Shift+V` 等，需要用 `global-hotkey` crate 的语法，例如 `"CommandOrControl+Shift+V"`）。

## 项目结构

```
clipmate/
├── src-tauri/
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   ├── build.rs
│   ├── capabilities/default.json
│   ├── icons/icon.png
│   └── src/main.rs        # 全部 Rust 逻辑（剪贴板、热键、命令、CGEvent）
├── ui/
│   ├── index.html
│   ├── style.css
│   └── app.js             # 全部前端逻辑（vanilla JS）
└── README.md
```

后端单文件 ~450 行；前端 ~120 行 JS + 130 行 CSS，刻意保持精简。
