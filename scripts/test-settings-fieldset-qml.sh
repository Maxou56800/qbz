#!/usr/bin/env bash
# SettingsFieldset regression: the collapsible options box of Appearance >
# Theme shows its rows inside the border, folds them on a header click or key,
# moves the settings below, and leaves the persisted state to its host. Same
# run in CI and locally.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
if [[ -n "${QT_ROOT_DIR:-}" ]]; then
  qt_fieldset_bins="$QT_ROOT_DIR/bin"
else
  qt_fieldset_qmake="${QMAKE:-}"
  if [[ -z "$qt_fieldset_qmake" ]]; then
    qt_fieldset_qmake="$(command -v qmake6 || command -v qmake)"
  fi
  qt_fieldset_bins="$("$qt_fieldset_qmake" -query QT_INSTALL_BINS)"
fi
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
  "$qt_fieldset_bins/qmltestrunner" \
  -input scripts/qml-tests/tst_settings_fieldset.qml \
  -import scripts/qml-tests/imports
