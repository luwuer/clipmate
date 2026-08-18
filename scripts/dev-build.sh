#!/bin/bash
# ClipMate 快速开发构建：debug 增量编译（秒级）+ 直接组装 .app
# 用法：./scripts/dev-build.sh [--run]
set -euo pipefail
cd "$(dirname "$0")/../src-tauri"

echo "→ cargo build (debug, 增量)…"
touch build.rs  # force generate_context! 重新读取 ui/ 嵌入前端
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
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>0.1.0</string>
  <key>LSUIElement</key><true/>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

codesign -f -s - --deep "$APP" 2>/dev/null
echo "✓ $APP (debug)"

if [ "${1:-}" = "--run" ]; then
  pkill -f "ClipMate.app/Contents/MacOS/clipmate" 2>/dev/null || true
  sleep 0.5
  open "$APP"
  echo "→ 已启动（日志: Console.app 搜 clipmate，或终端跑 $(cd ../dist && pwd)/ClipMate.app/Contents/MacOS/clipmate）"
fi
