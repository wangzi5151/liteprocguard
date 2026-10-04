#!/data/data/com.termux/files/usr/bin/bash
# ---------------------------------------------------------------------------
# Termux（Android aarch64/arm）一键编译 / 启动脚本
#
#   一键编译：  bash build-termux.sh
#   编译并运行： bash build-termux.sh run
#   编译并守护： bash build-termux.sh guard
#
# 该脚本会自动安装 Rust（若缺失），然后编译适配当前设备的单文件二进制。
# 纯本地编译，不下载任何预编译二进制、不上传任何数据。
# ---------------------------------------------------------------------------
set -euo pipefail
cd "$(dirname "$0")"

if ! command -v cargo >/dev/null 2>&1; then
  echo ">> 未检测到 Rust，正在通过 Termux 安装（pkg install rust）..."
  pkg update -y || true
  pkg install -y rust binutils
fi

echo ">> 正在编译 Release 版本（首次编译较慢，请耐心等待）..."
cargo build --release

BIN="$(pwd)/target/release/liteprocguard"
echo
echo ">> 编译完成：$BIN"
echo ">> 可直接运行： $BIN"

case "${1:-}" in
  run)
    echo ">> 启动交互式菜单"
    exec "$BIN"
    ;;
  guard)
    echo ">> 启动守护（Ctrl+C 停止并释放限制）"
    exec "$BIN" guard start -i 2
    ;;
  *)
    ;;
esac
