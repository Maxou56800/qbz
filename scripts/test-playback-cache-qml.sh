#!/usr/bin/env bash
# The same bounded view/activation regression runs in CI and locally.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
if [[ -n "${QT_ROOT_DIR:-}" ]]; then
  qt_playback_cache_bins="$QT_ROOT_DIR/bin"
else
  qt_playback_cache_qmake="${QMAKE:-}"
  if [[ -z "$qt_playback_cache_qmake" ]]; then
    qt_playback_cache_qmake="$(command -v qmake6 || command -v qmake)"
  fi
  qt_playback_cache_bins="$("$qt_playback_cache_qmake" -query QT_INSTALL_BINS)"
fi
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
  "$qt_playback_cache_bins/qmltestrunner" \
  -input scripts/qml-tests/tst_playback_cache.qml \
  -import scripts/qml-tests/imports
