#!/usr/bin/env bash
# Shared Linux CI/local runtime gate. Serial builds, no personal profile.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
export QBZ_PREBUILT_SHADERS=1
bash scripts/test-search-local-qml.sh
bash scripts/test-updates-qml.sh
bash scripts/test-playback-cache-qml.sh
bash scripts/test-text-edit-menu-qml.sh
bash scripts/test-compact-seek-qml.sh
bash scripts/test-library-folders-qml.sh
bash scripts/test-array-model-qml.sh
bash scripts/test-keyed-model-qml.sh
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
python3 scripts/qt-cargo.py test --manifest-path crates/Cargo.toml -p qbz-qt --no-fail-fast
target_dir="${CARGO_TARGET_DIR:-$PWD/crates/target}"
logs="$(mktemp -d "${TMPDIR:-/tmp}/qbz-qt-gate-XXXXXX")"
printf '[qt-gate] startup logs: %s\n' "$logs"
for profile in debug release; do
  args=()
  [[ "$profile" == release ]] && args=(--release)
  python3 scripts/qt-cargo.py build "${args[@]}" --manifest-path crates/Cargo.toml -p qbz-qt
  python3 scripts/qt-smoke.py "$target_dir/$profile/qbz" --log "$logs/$profile.log"
done
# A healthy offscreen bus cannot expose Qt's synchronous xcb D-Bus startup.
python3 scripts/qt-smoke.py "$target_dir/release/qbz" --silent-bus --log "$logs/release-silent-bus.log"
