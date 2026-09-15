#!/usr/bin/env bash
# The same bounded view/activation regression runs in CI and locally.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
if [[ -n "${QT_ROOT_DIR:-}" ]]; then
  qt_remote_volume_bins="$QT_ROOT_DIR/bin"
else
  qt_remote_volume_qmake="${QMAKE:-}"
  if [[ -z "$qt_remote_volume_qmake" ]]; then
    qt_remote_volume_qmake="$(command -v qmake6 || command -v qmake)"
  fi
  qt_remote_volume_bins="$("$qt_remote_volume_qmake" -query QT_INSTALL_BINS)"
fi
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
  "$qt_remote_volume_bins/qmltestrunner" \
  -input scripts/qml-tests/tst_remote_volume.qml \
  -import scripts/qml-tests/imports
