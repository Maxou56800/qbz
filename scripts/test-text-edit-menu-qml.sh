#!/usr/bin/env bash
# The same bounded view/activation regression runs in CI and locally.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
if [[ -n "${QT_ROOT_DIR:-}" ]]; then
  qt_text_edit_menu_bins="$QT_ROOT_DIR/bin"
else
  qt_text_edit_menu_qmake="${QMAKE:-}"
  if [[ -z "$qt_text_edit_menu_qmake" ]]; then
    qt_text_edit_menu_qmake="$(command -v qmake6 || command -v qmake)"
  fi
  qt_text_edit_menu_bins="$("$qt_text_edit_menu_qmake" -query QT_INSTALL_BINS)"
fi
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
  "$qt_text_edit_menu_bins/qmltestrunner" \
  -input scripts/qml-tests/tst_text_edit_menu.qml \
  -import scripts/qml-tests/imports
