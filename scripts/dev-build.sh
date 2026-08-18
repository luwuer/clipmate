#!/bin/bash
# ClipMate 快速开发构建：debug 增量编译（秒级）+ 直接组装 .app
# 用法：./scripts/dev-build.sh [--run]
set -euo pipefail
cd "$(dirname "$0")/.."

echo "→ 构建前端 ui-v3 (vite)…"
cd ui-v3
if [ ! -d node_modules ]; then
  npm install --registry=https://registry.npmmirror.com
fi
npm run build
cd ../src-tauri

echo "→ cargo build (debug, 增量)…"
touch build.rs  # force generate_context! 重新读取 ui-v3/dist 嵌入前端
cargo build

APP="../dist/ClipMate.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/debug/clipmate "$APP/Contents/MacOS/clipmate"
cp icons/icon.icns "$APP/Contents/Resources/icon.icns"

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>clipmate</string>
  <key>CFBundleIdentifier</key><string>com.mdy.clipmate</string>
  <key>CFBundleName</key><string>ClipMate</string>
  <key>CFBundleDisplayName</key><string>ClipMate</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.2.0</string>
  <key>CFBundleVersion</key><string>0.2.0</string>
  <key>LSUIElement</key><true/>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# 固定签名身份：cdhash 稳定 → TCC 辅助功能授权一次永久有效。
# 没有该身份则回退 ad-hoc（每次构建 cdhash 都变，授权会失效）。
SIGN_IDENTITY="ClipMate Dev"
if security find-identity -v -p codesigning | grep -q "$SIGN_IDENTITY"; then
  codesign -f -s "$SIGN_IDENTITY" --deep "$APP"
  echo "✓ $APP (debug, 固定签名 \"$SIGN_IDENTITY\")"
else
  codesign -f -s - --deep "$APP" 2>/dev/null
  echo "✓ $APP (debug, ad-hoc 回退签名)"
  echo "⚠️  钥匙串中未找到 \"$SIGN_IDENTITY\" 签名身份，已回退 ad-hoc 签名。"
  echo "    ad-hoc 每次构建 cdhash 都变 → 辅助功能授权会失效、列表条目消失！"
  echo "    请先执行一次:  bash scripts/setup-codesign.sh"
  echo "    并按其输出的 security import 命令把证书导入钥匙串。"
fi

if [ "${1:-}" = "--run" ]; then
  pkill -f "ClipMate.app/Contents/MacOS/clipmate" 2>/dev/null || true
  sleep 0.5
  open "$APP"
  echo "→ 已启动（日志: Console.app 搜 clipmate，或终端跑 $(cd ../dist && pwd)/ClipMate.app/Contents/MacOS/clipmate）"
fi
