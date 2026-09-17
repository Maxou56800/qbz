#!/usr/bin/env bash
# The same bounded view/activation regression runs in CI and locally.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
if [[ -n "${QT_ROOT_DIR:-}" ]]; then
  qt_miniplayer_bins="$QT_ROOT_DIR/bin"
else
  qt_miniplayer_qmake="${QMAKE:-}"
  if [[ -z "$qt_miniplayer_qmake" ]]; then
    qt_miniplayer_qmake="$(command -v qmake6 || command -v qmake)"
  fi
  qt_miniplayer_bins="$("$qt_miniplayer_qmake" -query QT_INSTALL_BINS)"
fi
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
  "$qt_miniplayer_bins/qmltestrunner" \
  -input scripts/qml-tests/tst_miniplayer.qml \
  -import scripts/qml-tests/imports
