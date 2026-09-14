#!/usr/bin/env bash
# Excel-style multi-select regression: every select-mode click target — row
# body, checkbox disc, checkbox controls, album cards, the name links on a
# row, the My QBZ rows, the folder rail and the settings folder table — hands
# Shift / Ctrl to the selection rule, and the rule itself ranges over string
# and numeric ids alike. Same run in CI and locally.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
if [[ -n "${QT_ROOT_DIR:-}" ]]; then
  qt_multi_select_bins="$QT_ROOT_DIR/bin"
else
  qt_multi_select_qmake="${QMAKE:-}"
  if [[ -z "$qt_multi_select_qmake" ]]; then
    qt_multi_select_qmake="$(command -v qmake6 || command -v qmake)"
  fi
  qt_multi_select_bins="$("$qt_multi_select_qmake" -query QT_INSTALL_BINS)"
fi
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
  "$qt_multi_select_bins/qmltestrunner" \
  -input scripts/qml-tests/tst_multi_select.qml \
  -import scripts/qml-tests/imports
