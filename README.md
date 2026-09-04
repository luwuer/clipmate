# ClipMate

一个极简的剪贴板历史工具。

- **F2**（可配置）唤起面板，方向键选择，Enter 粘贴回当前应用，Esc 关闭
- 记录 **文本** 与 **图片**，支持 **搜索过滤**
- **历史持久化**：重启后历史不丢失（JSONL 落盘，图片仅内存）
- **收藏/置顶**：⌘P pin 重要条目，永不被淘汰
- **多选批量**：Shift/⌘ 多选，批量拼接粘贴、批量删除
- **主题切换**：dark / light，菜单栏一键切换
- **开机自启**：菜单栏勾选即可
- 圆角悬浮面板，跟随光标位置弹出，可拖拽移动

## 效果预览

```
╭──────────────────────────────────╮
│ ClipMate                          │  ← 可拖拽标题栏
│ 搜索剪贴板历史…              ⌫   │  ← 搜索框 + 清空
├──────────────────────────────────┤
│ 📌 常用地址（置顶）                │
│ ✓ https://example.com/api/v1/... │  ← 多选中的条目带 ✓
│ 订单号 #2026-0818-0001            │
│ hello clipmate 第一条              │
├──────────────────────────────────┤
│ ↑↓ 选择  ⇧/⌘ 多选  ⏎ 粘贴  ⌘P 置顶 │  ← 快捷键提示栏
╰──────────────────────────────────╯
```

## 技术栈

- **Tauri 2** + **Rust**：后端逻辑、全局热键、剪贴板监听、Cmd+V 模拟、NSPanel 焦点模型
- **Vue 3 + Vite**：前端面板（ui/）
- 依赖：arboard（剪贴板读写）、core-graphics（CGEvent）、png、objc2（NSStatusItem / NSPanel）、tauri-plugin-global-shortcut

## 快速开始

```bash
# 开发构建（秒级增量，自动签名 + 组装 .app）
bash scripts/dev-build.sh --run

# 发布构建（见下文「发布打包」）
```

首次构建前端需 `npm install`（dev-build.sh 会自动处理）。

## 键盘操作

| 按键 | 动作 |
| --- | --- |
| **F2**（可配置） | 显示/隐藏面板 |
| **↑ / ↓** | 移动高亮 |
| **Shift+↑ / ↓** | 从锚点扩展连续多选 |
| **⌘+↑ / ↓** | 切换当前条目的选中状态 |
| **Enter** | 无多选：粘贴当前条目；有多选：按列表顺序拼接批量粘贴 |
| **Delete / Backspace** | 无多选：删除当前条目；有多选：批量删除（搜索框聚焦时 Backspace 只删文本） |
| **⌘P** | 置顶/取消置顶当前条目 |
| **Esc** | 先清除多选；无多选时关闭面板 |
| **输入文字** | 实时过滤历史（文本内容） |

## 菜单栏

菜单栏图标常驻，菜单项：

- **显示面板** — 等价于按热键
- **申请辅助权限并打开设置** — 触发系统授权请求 + 打开设置页
- **切换主题** — dark ↔ light，立即生效并持久化
- **开机自启** — 勾选后写入 LaunchAgent，登录时自动启动
- **退出** — 退出前自动落盘历史

## 配置（settings.json）

配置文件路径：`~/Library/Application Support/com.mdy.clipmate/settings.json`
首次启动自动生成，缺失或非法字段自动回退默认值。

| 字段 | 取值 | 默认 | 说明 |
| --- | --- | --- | --- |
| `hotkey` | 字符串 | `"F2"` | 全局唤起热键，`global-hotkey` 语法，如 `"CommandOrControl+Shift+V"` |
| `theme` | `"dark"` / `"light"` | `"dark"` | 主题；菜单栏切换会自动写回 |

历史数据：同目录 `history.jsonl`（文本条目，图片仅保留在内存中）。

## 首次使用：授权

粘贴到其他应用需要 macOS **辅助功能** 权限。ClipMate 会在启动时自动请求一次（系统弹窗）。如果设置列表里找不到 ClipMate（ad-hoc 重签名会导致旧条目失效）：

**方式 A（推荐，根治）**：使用固定签名身份构建，授权一次永久有效：

```bash
bash scripts/setup-codesign.sh   # 生成自签证书 CN=ClipMate Dev
# 按脚本输出的 security import 命令导入钥匙串，之后 dev-build 自动使用该身份
```

**方式 B**：系统设置 → 隐私与安全性 → 辅助功能 → 列表底部 **+** → 手动添加 ClipMate.app → 勾选。

**方式 C**：重置后重启触发重新弹窗：

```bash
tccutil reset Accessibility com.mdy.clipmate
```

菜单栏「申请辅助权限并打开设置」会同时触发系统请求并打开设置页（兼容 macOS 13 与 15 路径）。

## 行为细节

- **去重**：复制相同内容不产生重复条目，命中的条目提升到最前
- **持久化**：文本历史 JSONL 落盘（2s 防抖），退出时 final flush；图片仅内存
- **置顶保护**：达到 300 条上限时，置顶条目永不淘汰，非置顶保留最新
- **过滤**：搜索时输入「图片」「image」「img」「png」可筛选图片
- **限制**：单条文本最大 2 MB，单张图片最大 8 MB，最多保留 300 条
- **不记录**：剪贴板上的「文件」、「空白内容」会跳过
- **面板定位**：优先跟随文本插入点（caret），其次焦点元素，最后鼠标位置
- **测试模式**：`CLIPMATE_TEST_CENTER=1` 环境变量让面板固定在主屏中央（便于截图/调试）

## 发布打包

```bash
cd app
cargo build --release
npx -y @tauri-apps/cli@2 build   # 必须锁 v2（v3 schema 不兼容）

# 确认 LSUIElement + 签名（固定身份 "ClipMate Dev"，无则 ad-hoc 回退）
/usr/libexec/PlistBuddy -c "Add :LSUIElement bool true" \
  target/release/bundle/macos/ClipMate.app/Contents/Info.plist 2>/dev/null || true
codesign -f -s "ClipMate Dev" --deep target/release/bundle/macos/ClipMate.app \
  || codesign -f -s - --deep target/release/bundle/macos/ClipMate.app

# 制作 dmg
cd target/release/bundle
rm -rf dmg_staging && mkdir dmg_staging
cp -R macos/ClipMate.app dmg_staging/
ln -s /Applications dmg_staging/Applications
hdiutil create -volname "ClipMate" -srcfolder dmg_staging -ov -format UDZO ClipMate.dmg
rm -rf dmg_staging
```

产物集中到 `dist/`：`ClipMate.app` + `ClipMate.dmg`（含「拖到 Applications」引导）。

## 项目结构

```
clipmate/
├── app/
│   ├── Cargo.toml / tauri.conf.json / build.rs
│   └── src/
│       ├── main.rs        # 薄入口、settings.json 读写
│       ├── model.rs       # 数据模型 + 去重/上限纯逻辑（含单测）
│       ├── clipboard.rs   # NSPasteboard 轮询捕获
│       ├── paste.rs       # Cmd+V 模拟 + AX 权限
│       ├── panel.rs       # NSPanel 焦点模型 + caret 定位
│       ├── commands.rs    # Tauri commands
│       ├── storage.rs     # JSONL 持久化
│       ├── menubar.rs     # NSStatusItem 菜单
│       └── autostart.rs   # LaunchAgent 开机自启
├── ui/                    # Vue 3 + Vite 前端（App.vue + style.css）
├── scripts/
│   ├── dev-build.sh       # 秒级增量构建 + 签名
│   └── setup-codesign.sh  # 生成固定签名证书
└── CHANGELOG.md
```
