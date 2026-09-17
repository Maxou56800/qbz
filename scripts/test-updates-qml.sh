#!/usr/bin/env bash
# The same bounded view/activation regression runs in CI and locally.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
if [[ -n "${QT_ROOT_DIR:-}" ]]; then
  qt_updates_bins="$QT_ROOT_DIR/bin"
else
  qt_updates_qmake="${QMAKE:-}"
  if [[ -z "$qt_updates_qmake" ]]; then
    qt_updates_qmake="$(command -v qmake6 || command -v qmake)"
  fi
  qt_updates_bins="$("$qt_updates_qmake" -query QT_INSTALL_BINS)"
fi
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
  "$qt_updates_bins/qmltestrunner" \
  -input scripts/qml-tests/tst_updates.qml \
  -import scripts/qml-tests/imports
