#!/usr/bin/env bash
# CardMenu submenu regression: hovering the child keeps the pair open,
# leaving both closes both, a pick in the child forwards and closes all.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
if [[ -n "${QT_ROOT_DIR:-}" ]]; then
  qt_card_menu_bins="$QT_ROOT_DIR/bin"
else
  qt_card_menu_qmake="${QMAKE:-}"
  if [[ -z "$qt_card_menu_qmake" ]]; then
    qt_card_menu_qmake="$(command -v qmake6 || command -v qmake)"
  fi
  qt_card_menu_bins="$("$qt_card_menu_qmake" -query QT_INSTALL_BINS)"
fi
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
  "$qt_card_menu_bins/qmltestrunner" \
  -input scripts/qml-tests/tst_card_menu.qml \
  -import scripts/qml-tests/imports
