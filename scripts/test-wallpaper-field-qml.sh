#!/usr/bin/env bash
# WallpaperField regression: the Wallpaper background's crop follows the
# window — Qt's position on X11/macOS/Windows, the compositor's rectangle for
# this window on Plasma Wayland, centred otherwise — and an invisible field
# reads nothing. Same run in CI and locally.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."
if [[ -n "${QT_ROOT_DIR:-}" ]]; then
  qt_wallpaper_bins="$QT_ROOT_DIR/bin"
else
  qt_wallpaper_qmake="${QMAKE:-}"
  if [[ -z "$qt_wallpaper_qmake" ]]; then
    qt_wallpaper_qmake="$(command -v qmake6 || command -v qmake)"
  fi
  qt_wallpaper_bins="$("$qt_wallpaper_qmake" -query QT_INSTALL_BINS)"
fi
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software \
  "$qt_wallpaper_bins/qmltestrunner" \
  -input scripts/qml-tests/tst_wallpaper_field.qml \
  -import scripts/qml-tests/imports
