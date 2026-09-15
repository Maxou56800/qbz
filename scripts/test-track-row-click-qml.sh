#!/usr/bin/env bash
# Track-row click policy (#790): a single click on a desktop track row's body
# does nothing, a double click on any non-control part plays once, the play disc
# plays at once; select mode, dead rows, drag and kiosk tap-to-play are kept.
# Same run in CI and locally.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
if [[ -n "${QT_ROOT_DIR:-}" ]]; then
  qt_track_row_click_bins="$QT_ROOT_DIR/bin"
else
  qt_track_row_click_qmake="${QMAKE:-}"
  if [[ -z "$qt_track_row_click_qmake" ]]; then
    qt_track_row_click_qmake="$(command -v qmake6 || command -v qmake)"
  fi
  qt_track_row_click_bins="$("$qt_track_row_click_qmake" -query QT_INSTALL_BINS)"
fi
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
  "$qt_track_row_click_bins/qmltestrunner" \
  -input scripts/qml-tests/tst_track_row_click.qml \
  -import scripts/qml-tests/imports
