#!/usr/bin/env bash
#
# build-aarch64-linux.sh — build the Linux aarch64 (ARM64) `qbz` binary (the
# Qt frontend, package `qbz-qt`), e.g. for a Raspberry Pi 4 / other ARM boxes.
#
# Rewritten 2026-08-25 for the Slint -> Qt frontend switch. There is no memory
# wall any more: `qbz-qt` never pulls the 30 GB `qbz-ui` unit, so a 4 GB Pi
# CAN build it natively (slowly). What the Qt build needs instead is a
# **Qt >= 6.8 with private headers** — the cxx-qt RHI items include
# <rhi/qrhi.h> — and the QtQuick.Effects / Qt.labs.qmlmodels QML modules at
# run time. That is the distro gate:
#   Debian 13+ (6.8), Ubuntu 25.04+ (6.8), Fedora 41+ (6.8), Arch/Gentoo: OK
#   Ubuntu 24.04 (6.4, no Effects module), Debian 12, Fedora 40: NOT buildable
#   with distro Qt — use the CI AppImage (Qt bundled) or aqt on a bigger box.
#
# Modes, auto-selected by the host arch:
#
#   1. NATIVE  (running ON aarch64 Linux: a Pi, an ARM VM, the ARM CI runner)
#        -> apt-installs the build deps (Qt from the distro), installs rustup
#        if there is no cargo, runs `cargo build --release -p qbz-qt`, then
#        prints the glibc floor and boots the binary offscreen (same criterion
#        as CI: >= 10 log lines, zero QML complaints, QbzCore initialized).
#        This mirrors .github/workflows/build-qt-linux.yml's aarch64 job, with
#        distro Qt instead of aqt.
#
#   2. CROSS   (on x86-64 Linux with Docker) — NOT SUPPORTED for the Qt binary.
#        cxx-qt-build runs the TARGET Qt's `qmake -query` during the build,
#        which needs qemu-user + binfmt_misc on the host, and cross-rs's
#        images are Ubuntu 20.04 (Qt 5). A trixie-based cross image is
#        possible but untested; until someone builds and proves it, this mode
#        exits with that explanation instead of pretending. The qbzd script
#        keeps its CROSS mode — the daemon links no Qt.
#
# Memory: with <= 4 GB RAM the script forces CARGO_BUILD_JOBS=1 (one rustc at
# a time). Expect 1-3 h on a Pi 4 cold. Override with JOBS=.
#
# Output: dist/qbz-aarch64-linux (an aarch64 ELF — verify with `file`).
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

cd "$(dirname "$0")/.."          # repo root
REPO="$(pwd)"
OUT="$REPO/dist/qbz-aarch64-linux"
say() { printf '[aarch64-build] %s\n' "$*"; }
die() { say "ERROR: $*" >&2; exit 1; }

# Build deps for the Qt `qbz` binary — the same list as build-qt-linux.yml
# minus what aqt provides there (here the distro's Qt provides it).
DEPS=(
  build-essential pkg-config cmake clang libclang-dev nasm curl python3
  libasound2-dev libjack-jackd2-dev libdbus-1-dev libssl-dev
  libgl1-mesa-dev libegl1-mesa-dev libxkbcommon-dev
  qt6-base-dev qt6-base-private-dev
  qt6-declarative-dev qt6-declarative-private-dev
  qt6-shadertools-dev
  qml6-module-qtquick qml6-module-qtquick-controls qml6-module-qtquick-effects
  qml6-module-qtquick-window qml6-module-qtqml-models qml6-module-qt-labs-qmlmodels
  qt6-wayland qt6-qpa-plugins
)

arch="$(uname -m)"
case "$arch" in
  aarch64 | arm64)
    say "NATIVE build on $arch ($(. /etc/os-release; echo "$PRETTY_NAME"))"
    if command -v apt-get >/dev/null; then
      # The distro gate, checked before installing anything: a Qt below 6.8
      # fails much later with a confusing "non-existent property" at runtime.
      qtver="$(apt-cache policy qt6-base-dev | awk '/Candidate/ {print $2}' | grep -oE '^[0-9]+\.[0-9]+' || true)"
      [[ -n "$qtver" ]] || die "no qt6-base-dev candidate — this distro has no Qt 6 (see header)"
      if [[ "$(printf '%s\n' "$qtver" "6.8" | sort -V | head -1)" != "6.8" ]]; then
        die "distro Qt is $qtver; the tree needs >= 6.8 (see header for the distro table)"
      fi
      say "distro Qt $qtver — OK"
      sudo apt-get update
      sudo apt-get install -y --no-install-recommends "${DEPS[@]}"
    else
      say "non-apt distro: install the equivalents of: ${DEPS[*]}" >&2
    fi
    if ! command -v cargo >/dev/null && [[ ! -x "$HOME/.cargo/bin/cargo" ]]; then
      say "no cargo — installing rustup (stable, minimal)"
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
    fi
    export PATH="$HOME/.cargo/bin:$PATH"
    rustc -V; cargo -V

    mem_mb="$(awk '/MemTotal/ {print int($2/1024)}' /proc/meminfo)"
    if [[ -n "${JOBS:-}" ]]; then jobs="$JOBS"
    elif (( mem_mb <= 4500 )); then jobs=1
    else jobs="$(nproc)"; fi
    say "RAM ${mem_mb} MB -> CARGO_BUILD_JOBS=${jobs}"
    # No RUSTFLAGS, no mold, stable: same cache rule as qt-run.sh.
    # The target qt-cargo.py builds in (scripts/qt-target.py), resolved once.
    CARGO_TARGET_DIR="$(python3 scripts/qt-target.py resolve)"
    export CARGO_TARGET_DIR
    CARGO_BUILD_JOBS="$jobs" CARGO_INCREMENTAL=0 python3 scripts/qt-cargo.py build --release --manifest-path crates/Cargo.toml -p qbz-qt
    install -Dm755 "$CARGO_TARGET_DIR/release/qbz" "$OUT"
    ;;
  x86_64 | amd64)
    die "CROSS mode is not supported for the Qt binary (see the header: cxx-qt \
needs the target Qt's qmake at build time -> qemu-user + a Qt>=6.8 arm64 \
sysroot). Build natively on an ARM box, or dispatch release-linux-aarch64.yml."
    ;;
  *)
    die "unsupported build host arch: $arch"
    ;;
esac

say "done -> $OUT"
file "$OUT" || true
say "glibc floor: $(objdump -T "$OUT" | grep -oE 'GLIBC_[0-9]+\.[0-9]+' | sort -Vu | tail -1)"

# The smoke, same criterion as CI. Distro Qt is on the default paths, so no
# env is needed beyond the offscreen platform.
if [[ "${SMOKE:-1}" == 1 ]]; then
  log="$(mktemp "${TMPDIR:-/tmp}/qbz-aarch64-smoke-XXXXXX")"
  say "offscreen smoke (60 s) -> $log"
  QT_QPA_PLATFORM=offscreen RUST_LOG=info timeout 60 "$OUT" > "$log" 2>&1 || true
  lines="$(wc -l < "$log")"
  (( lines >= 10 )) || { cat "$log"; die "smoke: the app did not start ($lines lines)"; }
  errs="$(grep -av 'propertyCache' "$log" | grep -aciE 'is not a type|unavailable|ReferenceError|TypeError|Cannot read|Unable to assign|Cannot open|no such method|non-existent property|failed to load component|is not installed' || true)"
  if (( errs > 0 )); then
    grep -av 'propertyCache' "$log" | grep -aiE 'is not a type|unavailable|ReferenceError|TypeError|Cannot read|Unable to assign|Cannot open|no such method|non-existent property|failed to load component|is not installed' | head -20 >&2
    die "smoke: $errs QML complaint(s)"
  fi
  grep -aq 'QbzCore initialized' "$log" || { tail -20 "$log" >&2; die "smoke: never reached QbzCore init"; }
  say "smoke OK (0 QML complaints, core initialized)"
fi
