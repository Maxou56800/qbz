#!/usr/bin/env bash
# The same bounded view/activation regression runs in CI and locally.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
if [[ -n "${QT_ROOT_DIR:-}" ]]; then
  qt_compact_seek_bins="$QT_ROOT_DIR/bin"
else
  qt_compact_seek_qmake="${QMAKE:-}"
  if [[ -z "$qt_compact_seek_qmake" ]]; then
    qt_compact_seek_qmake="$(command -v qmake6 || command -v qmake)"
  fi
  qt_compact_seek_bins="$("$qt_compact_seek_qmake" -query QT_INSTALL_BINS)"
fi
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
  "$qt_compact_seek_bins/qmltestrunner" \
  -input scripts/qml-tests/tst_compact_seek.qml \
  -import scripts/qml-tests/imports
