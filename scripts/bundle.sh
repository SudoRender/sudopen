#!/bin/sh
# Builds target/<profile>/sudopen.app (ad-hoc signed).
# Usage: scripts/bundle.sh [release|debug]
set -eu
cd "$(dirname "$0")/.."
profile="${1:-release}"
if [ "$profile" = release ]; then cargo build --release -p sudopen -p sudopen-finder; else cargo build -p sudopen -p sudopen-finder; fi
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
app="target/$profile/sudopen.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "target/$profile/sudopen" "$app/Contents/MacOS/sudopen"
[ -f macos/AppIcon.icns ] || scripts/make-icon.sh >/dev/null
cp macos/AppIcon.icns "$app/Contents/Resources/AppIcon.icns"
sed "s/__VERSION__/$version/g" macos/Info.plist > "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"

# Finder extension (right-click "Open in sudopen").
appex="$app/Contents/PlugIns/sudopen-finder.appex"
mkdir -p "$appex/Contents/MacOS"
cp "target/$profile/sudopen-finder" "$appex/Contents/MacOS/sudopen-finder"
sed "s/__VERSION__/$version/g" macos/finder/Info.plist > "$appex/Contents/Info.plist"

# Sign inside out. Ad-hoc here; scripts/dmg.sh re-signs with Developer ID.
codesign --force --sign - --entitlements macos/finder/entitlements.plist "$appex" >/dev/null 2>&1
codesign --force --sign - "$app" >/dev/null 2>&1
echo "$app"
