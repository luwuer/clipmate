#!/bin/bash
# Clipmate 本地发布构建：release 编译 + 固定证书签名 → dist/Clipmate.app
# 与 release.sh 的区别：无需 Apple Developer 账号（不公证），适合本机日常安装。
# 用法：
#   bash scripts/build.sh            构建到 dist/Clipmate.app
#   bash scripts/build.sh --install  构建后安装到 /Applications 并启动
set -euo pipefail
cd "$(dirname "$0")/.."

echo "→ 1/4 构建前端 ui (vite)…"
cd ui
if [ ! -d node_modules ]; then
  npm install --registry=https://registry.npmmirror.com
fi
npm run build
cd ..

echo "→ 2/4 cargo build (release)…"
# force generate_context! 重新读取 ui/dist 嵌入前端
touch app/build.rs
cargo build --release --manifest-path app/Cargo.toml

echo "→ 3/4 组装 .app…"
VERSION=$(grep '"version"' app/tauri.conf.json | head -1 | sed 's/.*: "\(.*\)".*/\1/')
APP="dist/Clipmate.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp app/target/release/clipmate "$APP/Contents/MacOS/clipmate"
cp app/icons/icon.icns "$APP/Contents/Resources/icon.icns"
cp app/PrivacyInfo.xcprivacy "$APP/Contents/PrivacyInfo.xcprivacy" 2>/dev/null || true
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>clipmate</string>
  <key>CFBundleIdentifier</key><string>com.mdy.clipmate</string>
  <key>CFBundleName</key><string>Clipmate</string>
  <key>CFBundleDisplayName</key><string>Clipmate</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSUIElement</key><true/>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

echo "→ 4/4 代码签名…"
# 固定签名身份：cdhash 稳定 → TCC 辅助功能授权一次永久有效。
# ⚠️ CN 保留旧拼写 "ClipMate Dev"：与钥匙串现有证书一致，勿随应用名统一。
SIGN_IDENTITY="ClipMate Dev"
if security find-identity -v -p codesigning | grep -q "$SIGN_IDENTITY"; then
  codesign -f --deep --entitlements app/Entitlements.plist -s "$SIGN_IDENTITY" "$APP"
  codesign --verify --deep --strict "$APP"
  echo "✓ $APP (release ${VERSION}, 固定签名 \"$SIGN_IDENTITY\")"
else
  codesign -f -s - --deep "$APP" 2>/dev/null
  echo "✓ $APP (release ${VERSION}, ad-hoc 回退签名)"
  echo "⚠️  钥匙串中未找到 \"$SIGN_IDENTITY\" 签名身份，已回退 ad-hoc 签名。"
  echo "    ad-hoc 每次构建 cdhash 都变 → 辅助功能授权会失效、列表条目消失！"
  echo "    请先执行一次:  bash scripts/setup-codesign.sh 并按其输出导入证书。"
fi

if [ "${1:-}" = "--install" ]; then
  echo "→ 安装到 /Applications…"
  pkill -f "Clipmate.app/Contents/MacOS/clipmate" 2>/dev/null || true
  sleep 0.5
  rm -rf /Applications/Clipmate.app
  cp -R "$APP" /Applications/Clipmate.app
  open /Applications/Clipmate.app
  echo "✓ 已安装并启动 /Applications/Clipmate.app"
fi
