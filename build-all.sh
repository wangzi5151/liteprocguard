#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# LiteProcGuard 一键交叉编译脚本
#
# 一条命令批量产出全部平台二进制：
#   Windows amd64 / 386
#   Linux   amd64 / arm64 / armv7 / armv6（树莓派全系列）
#
# 依赖：只需要 Docker（推荐用 `cross`，自动处理各目标工具链与静态链接）。
#   cargo install cross --git https://github.com/cross-rs/cross
#
# 用法：
#   ./build-all.sh              构建全部目标
#   ./build-all.sh linux-arm64  只构建指定目标
#   NO_PACKAGE=1 ./build-all.sh 只编译，不打包压缩
# ---------------------------------------------------------------------------
set -euo pipefail

cd "$(dirname "$0")"

# 目标矩阵： 友好名|Rust 目标三元组|产物文件名
TARGETS=(
  "windows-amd64|x86_64-pc-windows-gnu|liteprocguard.exe"
  "windows-386|i686-pc-windows-gnu|liteprocguard.exe"
  "linux-amd64|x86_64-unknown-linux-musl|liteprocguard"
  "linux-arm64|aarch64-unknown-linux-musl|liteprocguard"
  "linux-armv7|armv7-unknown-linux-musleabihf|liteprocguard"
  "raspi-armv6|arm-unknown-linux-musleabihf|liteprocguard"
)

# 选择构建工具：优先 cross（自带 Docker 工具链），否则回退 cargo。
if command -v cross >/dev/null 2>&1; then
  BUILDER="cross"
  echo "使用 cross 构建（Docker 静态工具链）"
elif command -v cargo >/dev/null 2>&1; then
  BUILDER="cargo"
  echo "未找到 cross，回退到 cargo（需自行安装对应 target 与链接器）"
else
  echo "错误：未找到 cross 或 cargo，请先安装 Rust 工具链。" >&2
  exit 1
fi

FILTER="${1:-}"
DIST="dist"
mkdir -p "$DIST"

build_one() {
  local friendly="$1" triple="$2" bin="$3"
  echo "=============================================================="
  echo ">> 构建 $friendly ($triple)"
  echo "=============================================================="
  "$BUILDER" build --release --target "$triple"

  local src="target/${triple}/release/${bin}"
  if [ ! -f "$src" ]; then
    echo "!! 未找到产物 $src" >&2
    return 1
  fi

  local out="${DIST}/${friendly}"
  mkdir -p "$out"
  cp "$src" "${out}/${bin}"
  cp README.md LICENSE "$out/" 2>/dev/null || true
  cp -r presets "$out/" 2>/dev/null || true

  if [ "${NO_PACKAGE:-0}" != "1" ]; then
    ( cd "$DIST" && tar -czf "liteprocguard-${friendly}.tar.gz" "$friendly" )
    echo ">> 已打包 dist/liteprocguard-${friendly}.tar.gz"
  fi
  echo ">> 完成 $friendly -> ${out}/${bin}"
}

found=0
for entry in "${TARGETS[@]}"; do
  IFS='|' read -r friendly triple bin <<< "$entry"
  if [ -n "$FILTER" ] && [ "$FILTER" != "$friendly" ] && [ "$FILTER" != "$triple" ]; then
    continue
  fi
  found=1
  build_one "$friendly" "$triple" "$bin"
done

if [ "$found" -eq 0 ]; then
  echo "未匹配到目标：$FILTER" >&2
  echo "可用目标：" >&2
  for entry in "${TARGETS[@]}"; do
    IFS='|' read -r friendly triple _ <<< "$entry"
    echo "  $friendly  ($triple)"
  done
  exit 1
fi

echo
echo "全部完成，产物位于 dist/ 目录。"
ls -1 "$DIST"
