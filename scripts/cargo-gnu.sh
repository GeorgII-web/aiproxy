#!/usr/bin/env bash
# Runs cargo for the Windows GNU target inside WSL, with a MinGW toolchain unpacked (no sudo) into
# ~/.local/opt/mingw-w64 on first use. Builds a static exe without the Windows Rust toolchain.
# Usage: scripts/cargo-gnu.sh build --release   → target/x86_64-pc-windows-gnu/release/aiproxy.exe
set -euo pipefail
cd "$(dirname "$0")/.."
MINGW=${MINGW:-$HOME/.local/opt/mingw-w64}
if [ ! -x "$MINGW/usr/bin/x86_64-w64-mingw32-gcc-win32" ]; then
    debs=$(mktemp -d)
    (cd "$debs" && apt-get download binutils-mingw-w64-x86-64 gcc-mingw-w64-x86-64-win32 \
        gcc-mingw-w64-x86-64-win32-runtime gcc-mingw-w64-base mingw-w64-x86-64-dev mingw-w64-common)
    mkdir -p "$MINGW"
    for deb in "$debs"/*.deb; do dpkg -x "$deb" "$MINGW"; done
    rm -rf "$debs"
fi
export PATH="$MINGW/usr/bin:$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc-win32
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS="-C target-feature=+crt-static"
exec cargo "$1" --target x86_64-pc-windows-gnu "${@:2}"
