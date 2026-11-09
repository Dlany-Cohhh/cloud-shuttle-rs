#!/usr/bin/env bash
# 云梭 CloudShuttle · Rust 原生版 Linux / macOS 构建脚本
# 用法：bash scripts/build.sh [--static-dir PATH] [--out-dir PATH] [--debug] [--no-static]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

STATIC_DIR=""
OUT_DIR="$ROOT/dist/linux"
PROFILE="release"
NO_STATIC=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --static-dir) STATIC_DIR="$2"; shift 2 ;;
    --out-dir)    OUT_DIR="$2"; shift 2 ;;
    --debug)      PROFILE="debug"; shift ;;
    --no-static)  NO_STATIC=1; shift ;;
    -h|--help)    echo "用法: $0 [--static-dir PATH] [--out-dir PATH] [--debug] [--no-static]"; exit 0 ;;
    *) echo "未知参数: $1" >&2; exit 1 ;;
  esac
done

if [[ -z "$STATIC_DIR" ]]; then
  for c in "$ROOT/static" "$(dirname "$ROOT")/cloud-shuttle/static"; do
    if [[ -f "$c/index.html" ]]; then STATIC_DIR="$c"; break; fi
  done
fi

echo "==> cargo build ($PROFILE)"
if [[ "$PROFILE" == "debug" ]]; then cargo build; else cargo build --release; fi

BUILT="$ROOT/target/$PROFILE/cloud-shuttle-rs"
[[ -f "$BUILT" ]] || { echo "找不到编译产物：$BUILT" >&2; exit 1; }

mkdir -p "$OUT_DIR"
cp -f "$BUILT" "$OUT_DIR/cloud-shuttle-rs"
chmod +x "$OUT_DIR/cloud-shuttle-rs"

if [[ $NO_STATIC -eq 0 ]]; then
  if [[ -n "$STATIC_DIR" && -f "$STATIC_DIR/index.html" ]]; then
    mkdir -p "$OUT_DIR/static"
    cp -rf "$STATIC_DIR"/. "$OUT_DIR/static/"
    echo "==> 前端资源已复制：$OUT_DIR/static"
  else
    echo "==> 警告：未找到前端 static 目录，可用 --static-dir 指定；运行时也可用 --static-dir 指定。" >&2
  fi
fi

cp -f "$ROOT/.env.example" "$OUT_DIR/.env.example" 2>/dev/null || true
echo ""
echo "构建完成：$OUT_DIR"
echo "  自检：$OUT_DIR/cloud-shuttle-rs --check"
echo "  启动：$OUT_DIR/cloud-shuttle-rs"
