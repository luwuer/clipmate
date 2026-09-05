# Clipmate 发布指南

## 为什么不上 Mac App Store

Mac App Store 强制 **App Sandbox**，而沙盒中：
- 辅助功能 API（Accessibility）不可用
- CGEvent 事件注入（模拟 Cmd+V 粘贴）被禁止
- 全局热键监听受限

这三者正是 Clipmate 的核心能力。**同类工具（Maccy、CleanClip、Paste）均不在 App Store**，都采用 Developer ID 签名 + 公证的官网分发路径。Maccy 官方 FAQ 对此有明确说明。

## 分发路径：Developer ID + Notarization

用户下载 dmg 后双击即可打开，无 Gatekeeper 警告。准备工作：

### 1. 一次性准备（需要 Apple Developer 账号，$99/年）

1. **加入 Apple Developer Program**：https://developer.apple.com/programs/
2. **创建 Developer ID Application 证书**：Xcode → Settings → Accounts → Manage Certificates → "+" → Developer ID Application
3. **配置 notarytool 凭据**（一次性）：

```bash
xcrun notarytool store-credentials "clipmate-notary" \
  --apple-id "你的AppleID@example.com" \
  --team-id "你的TeamID" \
  --password "app-specific-password"   # 在 appleid.apple.com 生成
```

### 2. 每次发布

```bash
CLIPMATE_TEAM_ID=XXXXXXXXXX bash scripts/release.sh
```

脚本自动完成：release 构建 → Developer ID 签名（含 entitlements + privacy manifest）→ dmg 制作 → notarization 公证 → staple 票据。

产物：`dist/Clipmate-0.2.0.dmg`

### 3. 验证（可选）

```bash
# 签名验证
codesign --verify --deep --strict dist/Clipmate.app 2>/dev/null || \
  codesign --verify --deep --strict app/target/release/bundle/macos/Clipmate.app
# 公证验证
spctl -a -vv -t install dist/Clipmate-0.2.0.dmg
```

## 已就绪的发布材料

| 文件 | 用途 |
|---|---|
| `app/Entitlements.plist` | 签名 entitlements（非沙盒分发必需） |
| `app/PrivacyInfo.xcprivacy` | Apple 隐私清单（2024 起要求）：声明剪贴板访问用途、不追踪、本地存储 |
| `scripts/release.sh` | 一键发布脚本（签名+公证+打包） |
| `app/icons/` | 全套应用图标（icns/png） |

## 如果你仍想上 App Store（不建议）

理论上需要：重写为沙盒应用（放弃粘贴功能，只保留历史查看）+ Apple Distribution 证书 + Provisioning Profile + App Store Connect 元数据。**这会砍掉产品的核心价值**，不推荐。
