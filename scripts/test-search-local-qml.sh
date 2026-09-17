#!/usr/bin/env bash
# The same bounded view/activation regression runs in CI and locally.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
if [[ -n "${QT_ROOT_DIR:-}" ]]; then
  qt_search_bins="$QT_ROOT_DIR/bin"
else
  qt_search_qmake="${QMAKE:-}"
  if [[ -z "$qt_search_qmake" ]]; then
    qt_search_qmake="$(command -v qmake6 || command -v qmake)"
  fi
  qt_search_bins="$("$qt_search_qmake" -query QT_INSTALL_BINS)"
fi
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
  "$qt_search_bins/qmltestrunner" \
  -input scripts/qml-tests/tst_search_local.qml \
  -import scripts/qml-tests/imports
