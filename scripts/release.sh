#!/bin/bash
# ClipMate Developer ID 签名 + Notarization 公证 + 打包 dmg
# 用法：
#   1. 在 Xcode/钥匙串中已有 "Developer ID Application" 证书
#   2. 配置下方 TEAM_ID / SIGN_IDENTITY / NOTARY_PROFILE
#   3. bash scripts/release.sh
#
# NOTARY_PROFILE 是 notarytool 的钥匙串 profile（一次性配置）：
#   xcrun notarytool store-credentials "clipmate-notary" \
#     --apple-id "you@example.com" --team-id "XXXXXXXXXX" --password "app-specific-password"
set -euo pipefail
cd "$(dirname "$0")/.."

# ---------- 配置区（发布前填） ----------
TEAM_ID="${CLIPMATE_TEAM_ID:-}"
SIGN_IDENTITY="${CLIPMATE_SIGN_IDENTITY:-Developer ID Application}"
NOTARY_PROFILE="${CLIPMATE_NOTARY_PROFILE:-clipmate-notary}"
VERSION=$(grep '"version"' app/tauri.conf.json | head -1 | sed 's/.*: "\(.*\)".*/\1/')

if [ -z "$TEAM_ID" ]; then
  echo "❌ 未配置 TEAM_ID。用法：CLIPMATE_TEAM_ID=XXXXXXXXXX bash scripts/release.sh"
  echo "   （在 https://developer.apple.com/account 的 Membership 页面查看 Team ID）"
  exit 1
fi

echo "==> 1/5 release 构建"
cargo build --release --manifest-path app/Cargo.toml
npx -y @tauri-apps/cli@2 build --manifest-path app/Cargo.toml 2>&1 | tail -2

APP="app/target/release/bundle/macos/ClipMate.app"
DMG_OUT="dist/ClipMate-${VERSION}.dmg"

echo "==> 2/5 Developer ID 签名（$SIGN_IDENTITY）"
# 嵌套 entitlements + privacy manifest
cp app/Entitlements.plist /tmp/clipmate-ent.plist
cp app/PrivacyInfo.xcprivacy "$APP/Contents/PrivacyInfo.xcprivacy" 2>/dev/null || true
codesign --force --deep --options runtime \
  --entitlements /tmp/clipmate-ent.plist \
  --sign "$SIGN_IDENTITY" \
  --timestamp \
  "$APP"
codesign --verify --deep --strict --verbose=2 "$APP" 2>&1 | tail -2

echo "==> 3/5 制作 dmg"
rm -rf /tmp/clipmate-dmg-staging && mkdir -p /tmp/clipmate-dmg-staging
cp -R "$APP" /tmp/clipmate-dmg-staging/
ln -s /Applications /tmp/clipmate-dmg-staging/Applications
mkdir -p dist
hdiutil create -volname "ClipMate ${VERSION}" \
  -srcfolder /tmp/clipmate-dmg-staging \
  -ov -format UDZO "$DMG_OUT" >/dev/null
rm -rf /tmp/clipmate-dmg-staging
codesign --force --sign "$SIGN_IDENTITY" --timestamp "$DMG_OUT"

echo "==> 4/5 Notarization 公证（profile: $NOTARY_PROFILE）"
xcrun notarytool submit "$DMG_OUT" --keychain-profile "$NOTARY_PROFILE" --wait

echo "==> 5/5 Staple 公证票据"
xcrun stapler staple "$DMG_OUT"
xcrun stapler validate "$DMG_OUT"

echo ""
echo "✅ 发布产物：$DMG_OUT"
echo "   用户下载后双击打开，Gatekeeper 不拦截（已公证）。"
