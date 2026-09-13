#!/usr/bin/env bash
# The same bounded view/activation regression runs in CI and locally.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
if [[ -n "${QT_ROOT_DIR:-}" ]]; then
  qt_library_folders_bins="$QT_ROOT_DIR/bin"
else
  qt_library_folders_qmake="${QMAKE:-}"
  if [[ -z "$qt_library_folders_qmake" ]]; then
    qt_library_folders_qmake="$(command -v qmake6 || command -v qmake)"
  fi
  qt_library_folders_bins="$("$qt_library_folders_qmake" -query QT_INSTALL_BINS)"
fi
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
  "$qt_library_folders_bins/qmltestrunner" \
  -input scripts/qml-tests/tst_library_folders.qml \
  -import scripts/qml-tests/imports
