#!/usr/bin/env bash
# 构建并用固定自签名证书签名、部署 macOS 版
# 证书「ClipSync Dev」只创建一次（见 README），之后一直复用，
# 这样「辅助功能」授权不会因为重新打包而失效。
set -euo pipefail
cd "$(dirname "$0")/.."

IDENTITY="${CLIPSYNC_SIGN_IDENTITY:-ClipSync Dev}"
# 专用钥匙串（密码公开，仅为避免每次签名弹授权框）
KEYCHAIN="${CLIPSYNC_KEYCHAIN:-$HOME/Library/Keychains/clipsync-sign.keychain-db}"

# 重映射源码绝对路径，避免把本机用户名写进二进制
export RUSTC_WRAPPER="$PWD/scripts/rustc-remap.sh"

echo "==> 构建 macOS 应用"
npx tauri build

SRC="src-tauri/target/release/bundle/macos/ClipSync.app"
DST="$HOME/Applications/ClipSync.app"

echo "==> 关闭正在运行的实例"
osascript -e 'tell application "ClipSync" to quit' 2>/dev/null || true
pkill -f "ClipSync.app/Contents/MacOS" 2>/dev/null || true
sleep 1

echo "==> 部署到 $DST"
rm -rf "$DST"
cp -R "$SRC" "$DST"

echo "==> 使用「$IDENTITY」签名"
if [ -f "$KEYCHAIN" ]; then
  security unlock-keychain -p clipsync "$KEYCHAIN" >/dev/null 2>&1 || true
  codesign --force --deep --options runtime --keychain "$KEYCHAIN" --sign "$IDENTITY" "$DST"
else
  codesign --force --deep --options runtime --sign "$IDENTITY" "$DST"
fi
codesign -dvv "$DST" 2>&1 | grep -E "Identifier|Authority" || true

touch "$DST"
open "$DST"
echo "✅ 完成"
