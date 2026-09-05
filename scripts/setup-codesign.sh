#!/bin/bash
# ClipMate 固定代码签名证书生成脚本
#
# 为什么需要：TCC（辅助功能授权）按 (designated requirement + cdhash) 匹配条目。
# ad-hoc 签名（codesign -s -）每次打包 cdhash 都变 → 旧授权条目失效"消失"。
# 用固定自签证书签名后 cdhash 永久稳定 → 一次授权永久有效。
#
# 用法：
#   bash scripts/setup-codesign.sh            生成证书到 scripts/certs/，打印导入钥匙串命令（不自动执行）
#   bash scripts/setup-codesign.sh --dry-run  仅演示生成流程（临时目录，结束自动删除，不动 repo / 钥匙串）
set -euo pipefail
cd "$(dirname "$0")/.."

CN="ClipMate Dev"
P12_PASS="clipmate-dev"
DRY_RUN=false
[ "${1:-}" = "--dry-run" ] && DRY_RUN=true

if $DRY_RUN; then
  CERT_DIR="$(mktemp -d /tmp/clipmate-certs.XXXXXX)"
  trap 'rm -rf "$CERT_DIR"' EXIT
  echo "[dry-run] 证书将生成到临时目录 ${CERT_DIR}（脚本结束后自动删除）"
else
  CERT_DIR="scripts/certs"
  mkdir -p "$CERT_DIR"
fi

KEY="$CERT_DIR/clipmate-dev.key"
CRT="$CERT_DIR/clipmate-dev.crt"
P12="$CERT_DIR/clipmate-dev.p12"

echo "→ 生成 RSA 2048 私钥 + 自签证书（CN=\"$CN\"，codeSigning EKU，10 年）…"
openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout "$KEY" -out "$CRT" -days 3650 \
  -subj "/CN=$CN" \
  -addext "keyUsage=digitalSignature" \
  -addext "extendedKeyUsage=codeSigning" 2>&1 | grep -v '^+\{3,\}$' || true

echo "→ 导出 p12（导入钥匙串用，密码: ${P12_PASS}）…"
# -legacy：OpenSSL 3.x 默认用 AES-256-CBC/PBKDF2 导出，macOS security import 会报
# "MAC verification failed during PKCS12 import"。-legacy 走 RC2/SHA1 老算法，macOS 可读。
# 若本机 openssl 不支持 -legacy（老版本 LibreSSL 自带支持），去掉该参数即可。
openssl pkcs12 -export -legacy -out "$P12" -inkey "$KEY" -in "$CRT" -passout "pass:$P12_PASS" \
  || openssl pkcs12 -export -out "$P12" -inkey "$KEY" -in "$CRT" -passout "pass:$P12_PASS"

echo "→ 证书信息："
openssl x509 -in "$CRT" -noout -subject -dates -ext extendedKeyUsage

if $DRY_RUN; then
  echo ""
  echo "[dry-run] 完成。未写入 repo，未触碰钥匙串。正式生成请去掉 --dry-run 再跑一次。"
  exit 0
fi

chmod 600 "$KEY" "$P12"
echo ""
echo "✓ 证书已生成："
echo "    ${CRT}（公钥证书，可进 git）"
echo "    ${KEY} / ${P12}（私钥，已被 .gitignore 排除）"
echo ""
echo "═══ 接下来需要你手动执行一次（会弹 GUI 密码框）═══"
echo ""
echo "  # 1. 导入身份（若 p12 是老版本脚本生成的，先加 -legacy 重新导出再导入）"
echo "  security import \"$P12\" -k ~/Library/Keychains/login.keychain-db -P \"$P12_PASS\" -T /usr/bin/codesign"
echo ""
echo "  # 2. 设置用户级 codeSign 信任（自签证书默认 CSSMERR_TP_NOT_TRUSTED，不设信任"
echo "  #    find-identity -v 会显示 0 valid identities，codesign 找不到身份）"
echo "  security add-trusted-cert -r trustRoot -p codeSign -k ~/Library/Keychains/login.keychain-db \"$CRT\""
echo ""
echo "导入后验证（应能看到一行 \"$CN\"）："
echo ""
echo "  security find-identity -v -p codesigning | grep \"$CN\""
echo ""
echo "之后 bash scripts/dev-build.sh 会自动用该身份固定签名。"
