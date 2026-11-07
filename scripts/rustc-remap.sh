#!/bin/sh
# RUSTC_WRAPPER：cargo 会把真实 rustc 路径作为第一个参数传入
# 作用：重映射源码绝对路径，避免把本机用户名/目录写进二进制
REAL_RUSTC="$1"
shift

exec "$REAL_RUSTC" \
  --remap-path-prefix="$HOME=/build" \
  --remap-path-prefix="/Users/$(whoami)=/build" \
  "$@"
