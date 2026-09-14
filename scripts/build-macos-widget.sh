#!/bin/sh
set -eu

if [ "$(uname -s)" != "Darwin" ]; then
  exit 0
fi

widget_root="$(cd "$(dirname "$0")/../macos-widget" && pwd)"
developer_dir="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"

if [ ! -x "$developer_dir/usr/bin/xcodebuild" ]; then
  echo "Xcode is required to build the c9watch desktop widget." >&2
  exit 1
fi

cd "$widget_root"
xcodegen generate --spec project.yml
DEVELOPER_DIR="$developer_dir" xcodebuild \
  -project C9WatchWidgets.xcodeproj \
  -scheme C9WatchWidgets \
  -configuration Release \
  -derivedDataPath build/DerivedData \
  CODE_SIGNING_ALLOWED=NO \
  build >/dev/null

rm -rf build/c9watch-widget.appex
mkdir -p build
ditto \
  build/DerivedData/Build/Products/Release/C9WatchWidget.appex \
  build/c9watch-widget.appex

# Tauri signs the containing application later. Sign the extension here so the
# nested WidgetKit bundle is valid when it is copied into Contents/PlugIns.
codesign --force --sign - \
  --entitlements Widget/Widget.entitlements \
  build/c9watch-widget.appex/Contents/MacOS/C9WatchWidget >/dev/null
codesign --force --sign - \
  --entitlements Widget/Widget.entitlements \
  build/c9watch-widget.appex >/dev/null
