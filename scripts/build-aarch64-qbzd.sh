#!/usr/bin/env bash
#
# build-aarch64-qbzd.sh — build the Linux aarch64 (ARM64) `qbzd` daemon binary,
# e.g. for a Raspberry Pi 4 + HiFiBerry/USB DAC streamer.
#
# ── Why this is NOT build-aarch64-linux.sh ───────────────────────────────────
# That script builds the Qt desktop `qbz`, which needs a Qt >= 6.8 with private
# headers on the build host and cannot be cross-compiled with cross-rs (the
# target Qt's qmake has to run during the build). `qbzd` is the Qt-free and
# Slint-free column of the workspace: no UI crate, no fonts, no GPU libs. It
# builds in minutes on modest hardware and cross-compiles cleanly, so:
#   • a 4 GB Pi CAN build it natively;
#   • CROSS mode via cross-rs + crates/Cross.toml still works here;
#   • the container caps are small.
#
# Two modes, auto-selected by host arch (same shape as the desktop script):
#
#   1. NATIVE  (on aarch64 Linux: the Pi itself, an ARM VM/runner)
#   2. CROSS   (on x86-64 Linux with Docker, via `cross`; crates/Cross.toml
#              supplies the arm64 dev libs inside the image)
#
# Output: dist/qbzd-aarch64-linux (an aarch64 ELF — verify with `file`).
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

cd "$(dirname "$0")/.."          # repo root (qbz-nix)
REPO="$(pwd)"
TARGET="aarch64-unknown-linux-gnu"
OUT="$REPO/dist/qbzd-aarch64-linux"

# Native deps for the DAEMON only. Audio (ALSA/JACK; PipeWire is reached through
# its ALSA shim), D-Bus for MPRIS, TLS, and the usual -sys toolchain. The GUI
# stack the desktop needs (fontconfig, freetype, xkbcommon, wayland, xcb, GL,
# EGL) is deliberately absent — qbzd links none of it.
DEPS=(
  build-essential pkg-config cmake clang libclang-dev
  libasound2-dev libjack-jackd2-dev
  libdbus-1-dev libssl-dev
)

arch="$(uname -m)"
case "$arch" in
  aarch64 | arm64)
    echo "[qbzd-aarch64] NATIVE build on $arch"
    if command -v apt-get >/dev/null; then
      sudo apt-get update
      sudo apt-get install -y --no-install-recommends "${DEPS[@]}"
    else
      echo "[qbzd-aarch64] non-apt distro: install the equivalents of: ${DEPS[*]}" >&2
    fi
    # This worktree's own target (scripts/qt-target.py): Cargo's default in a
    # plain checkout, a per-worktree sibling when the host shares one target.
    CARGO_TARGET_DIR="$(python3 scripts/qt-target.py resolve)"
    export CARGO_TARGET_DIR
    ( cd crates && cargo build --release -p qbzd )
    install -Dm755 "$CARGO_TARGET_DIR/release/qbzd" "$OUT"
    ;;
  x86_64 | amd64)
    echo "[qbzd-aarch64] CROSS-compile from $arch via cross (Docker)"
    if ! docker info >/dev/null 2>&1; then
      echo "[qbzd-aarch64] ERROR: Docker is not running. cross needs it." >&2
      exit 1
    fi
    command -v cross >/dev/null || cargo install cross --locked
    # Modest caps: the heaviest qbzd rustc is a fraction of the desktop's, so
    # this can be capped tightly and still never OOM. The point of the cap is
    # the same as the desktop script's — a runaway gets killed instead of
    # swap-thrashing this box (30 GB, no hibernation) into a hard freeze.
    export CROSS_CONTAINER_OPTS="${CROSS_CONTAINER_OPTS:---memory=8g --memory-swap=12g}"
    # crates/Cross.toml injects the arm64 dev libs into the image (the daemon's
    # set; the Qt desktop is not cross-built, see build-aarch64-linux.sh).
    # cross mounts CARGO_TARGET_DIR into the container: the worktree's own
    # target here too, never one another worktree also builds into.
    CARGO_TARGET_DIR="$(python3 scripts/qt-target.py resolve)"
    export CARGO_TARGET_DIR
    ( cd crates && cross build --release --target "$TARGET" -p qbzd )
    install -Dm755 "$CARGO_TARGET_DIR/$TARGET/release/qbzd" "$OUT"
    ;;
  *)
    echo "[qbzd-aarch64] ERROR: unsupported build host arch: $arch" >&2
    exit 1
    ;;
esac

echo "[qbzd-aarch64] done -> $OUT"
file "$OUT" || true
