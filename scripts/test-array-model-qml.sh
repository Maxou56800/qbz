#!/usr/bin/env bash
# QbzArrayModel regression: a rows swap keeps the header's keyboard focus and
# the viewport (Qt 6.11 ListView.model assignment side effects). Same run in
# CI and locally.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
if [[ -n "${QT_ROOT_DIR:-}" ]]; then
  qt_array_model_bins="$QT_ROOT_DIR/bin"
else
  qt_array_model_qmake="${QMAKE:-}"
  if [[ -z "$qt_array_model_qmake" ]]; then
    qt_array_model_qmake="$(command -v qmake6 || command -v qmake)"
  fi
  qt_array_model_bins="$("$qt_array_model_qmake" -query QT_INSTALL_BINS)"
fi
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
  "$qt_array_model_bins/qmltestrunner" \
  -input scripts/qml-tests/tst_array_model.qml \
  -import scripts/qml-tests/imports
