#!/usr/bin/env bash
# QbzKeyedModel regression: rows reconcile into removes / inserts / moves /
# updates, a scope change resets silently at the top, a removal keeps the
# viewport and the delegates, pooled delegates come back opaque, and the
# Library grid keeps its cards and covers when an album leaves. Same run in CI
# and locally.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
if [[ -n "${QT_ROOT_DIR:-}" ]]; then
  qt_keyed_model_bins="$QT_ROOT_DIR/bin"
else
  qt_keyed_model_qmake="${QMAKE:-}"
  if [[ -z "$qt_keyed_model_qmake" ]]; then
    qt_keyed_model_qmake="$(command -v qmake6 || command -v qmake)"
  fi
  qt_keyed_model_bins="$("$qt_keyed_model_qmake" -query QT_INSTALL_BINS)"
fi
for suite in tst_keyed_model tst_library_keyed; do
  QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
    "$qt_keyed_model_bins/qmltestrunner" \
    -input "scripts/qml-tests/$suite.qml" \
    -import scripts/qml-tests/imports
done
