# Changelog

## [0.2.1] - 2026-09-05

Windows 支持首发 + GitHub 双平台 CI 发布链路。

### 功能

- **Windows 支持**（R22）：全功能原生实现——剪贴板监听（GetClipboardSequenceNumber）、粘贴（SendInput Ctrl+V，无需授权）、系统托盘（Tauri 2 TrayIcon）、开机自启（HKCU Run 注册表）、NSIS 安装包；与 macOS 对外接口（commands / 设置项 / 快捷键）完全一致
- **GitHub Actions 双平台 CI**：push main 产出 artifact（Windows NSIS + macOS Universal DMG），push tag `v*` 自动发布 Release（稳定文件名 `ClipMate_x64-setup.exe` / `ClipMate_universal.dmg`）
- **GitHub Pages 产品页**：`docs/index.html`，平台自动识别下载入口

### 修复

- `release.sh`：`tauri build --manifest-path` 参数不存在（CLI 无此参数），改为在 `app/` 目录内运行
- windows crate 0.61 适配：`BOOL` 迁移至 `windows::core`、`GetWindowRect` 返回 `Result`、`Win32_System_Registry`/`Win32_Security` feature 补全

## [0.2.0] - 2026-08-18

从 v0.1.0 起步的 21 轮迭代（R1–R21）全部合入，涵盖持久化、前端重写、主题系统、多选批量与开机自启。

### 功能

- **历史持久化**（R1）：剪贴板历史 JSONL 落盘 + 2s 防抖写入，重启后历史保留（图片仅内存）
- **收藏/置顶**（R2）：条目可 pin，置顶显示；⌘P 快捷键切换
- **菜单栏图标**（R3）：NSStatusItem 常驻菜单栏，含「申请辅助权限」「切换主题」「开机自启」「退出」入口
- **自定义热键**（R4）：`settings.json` 的 `hotkey` 字段可配置全局唤起键（默认 F2，缺失/非法自动回退）
- **Vue 3 前端重写**（R12）：ui-v3/（Vite + Vue3）替换 vanilla JS，圆角悬浮卡片 UI
- **主题切换**（R16）：dark / light 双主题，CSS 变量驱动；菜单栏一键切换，`settings.json` 的 `theme` 字段持久化
- **多选批量**（R19）：Shift+↑/↓ 连续多选、⌘+↑/↓ 逐项切换；Enter 批量拼接粘贴、Delete 批量删除；Esc 先清多选再关面板
- **开机自启**（R20）：菜单栏勾选开关，写入 LaunchAgent plist，路径漂移自动重写
- **标题栏拖拽**（R13）：面板可拖拽移动（nonactivating NSPanel 手工追踪）
- **caret 精确定位**（R10/R13）：AXBoundsForRange 取插入点，面板出现在光标下方；回退链 caret → 焦点元素 frame（带合理性过滤）→ 鼠标位置

### 修复与稳定

- **TCC 授权固定签名**（R12）：自签证书 `Clipmate Dev`，cdhash 稳定，辅助功能授权一次永久有效（ad-hoc 每次重签失效的根治方案）
- **pinned 永不淘汰**（R5）：上限截断保护置顶条目；落盘/加载统一 `split_for_limit`（R8）
- **去重与上限统一**（R6）：任意条目去重，命中提升 recency
- **透明圆角全链路**（R11–R18）：transparent + macos-private-api + html/body 透明 + 关闭系统阴影，四角视觉干净
- **键盘链路根治**（R6 前）：NSPanel canBecomeKeyWindow swizzle + WKWebView firstResponder，移除高风险的 CGEventTap 兜底（系统级卡死事故复盘）
- **空状态/圆角/拖拽等 UI 修复**（R9）
- **R21**：多选残留清理（refresh 后剪枝越界 selection/anchor）；搜索框聚焦时 Backspace 永不误删条目

### 工程

- **模块化重构**（R7）：main.rs 1074 行 → 薄入口，拆 model / clipboard / paste / panel / commands / storage / menubar / autostart
- **代码卫生**（R8）：clippy 清零，死代码清除，13/13 单测
- **dev-build.sh**：秒级增量构建 + 固定签名回退检测；`CLIPMATE_TEST_CENTER=1` 测试模式

## [0.1.0] - 2026-08-18

- 首个版本：F2 唤起剪贴板历史面板，文本/图片记录，搜索过滤，选中后 Cmd+V 模拟粘贴回当前应用
