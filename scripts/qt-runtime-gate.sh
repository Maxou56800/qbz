#!/usr/bin/env bash
# Shared Linux CI/local runtime gate. Serial builds, no personal profile.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
export QBZ_PREBUILT_SHADERS=1
# The same target qt-run.sh builds in: this worktree's own when the host shares
# one target across worktrees, Cargo's default otherwise (scripts/qt-target.py).
CARGO_TARGET_DIR="$(python3 scripts/qt-target.py resolve)"
export CARGO_TARGET_DIR
bash scripts/test-search-local-qml.sh
bash scripts/test-updates-qml.sh
bash scripts/test-playback-cache-qml.sh
bash scripts/test-text-edit-menu-qml.sh
bash scripts/test-compact-seek-qml.sh
bash scripts/test-remote-volume-qml.sh
bash scripts/test-library-folders-qml.sh
bash scripts/test-array-model-qml.sh
bash scripts/test-keyed-model-qml.sh
bash scripts/test-multi-select-qml.sh
bash scripts/test-track-row-click-qml.sh
bash scripts/test-settings-fieldset-qml.sh
bash scripts/test-wallpaper-field-qml.sh
bash scripts/test-card-menu-qml.sh
bash scripts/test-orbit-qml.sh
node scripts/test_qt_local_views.mjs
node scripts/test_qt_library.mjs
node scripts/test_qt_purchases_navigation.mjs
node scripts/test_qt_kiosk_art.mjs
node scripts/test_qt_kiosk_navigation.mjs
node scripts/test_qt_kiosk_feedback.mjs
node scripts/test_qt_exclusive_gate.mjs
# Volume handoff must retain coverage for unknown levels, capabilities and
# peer-to-peer switches. The full suite below executes these tests once.
volume_tests=$(python3 scripts/qt-cargo.py test --manifest-path crates/Cargo.toml -p qbz-qt -- --list 2>/dev/null | grep -c '::peer_volume_.*: test$' || true)
(( volume_tests >= 4 )) || { printf 'Missing QConnect volume regressions: %s/4\n' "$volume_tests"; exit 1; }
python3 scripts/qt-cargo.py test --manifest-path crates/Cargo.toml -p qbz-qt --no-fail-fast
target_dir="$CARGO_TARGET_DIR"
logs="$(mktemp -d "${TMPDIR:-/tmp}/qbz-qt-gate-XXXXXX")"
printf '[qt-gate] startup logs: %s\n' "$logs"
for profile in debug release; do
  args=()
  [[ "$profile" == release ]] && args=(--release)
  python3 scripts/qt-cargo.py build "${args[@]}" --manifest-path crates/Cargo.toml -p qbz-qt
  python3 scripts/qt-smoke.py "$target_dir/$profile/qbz" --log "$logs/$profile.log"
done
python3 scripts/qt-target.py link --target-dir "$target_dir" \
  || printf '[qt-gate] crates/target not re-pointed; binaries are in %s\n' "$target_dir" >&2
# A healthy offscreen bus cannot expose Qt's synchronous xcb D-Bus startup.
python3 scripts/qt-smoke.py "$target_dir/release/qbz" --silent-bus --log "$logs/release-silent-bus.log"
