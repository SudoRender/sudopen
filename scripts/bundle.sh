#!/bin/sh
# Builds target/<profile>/sudopen.app (ad-hoc signed).
# Usage: scripts/bundle.sh [release|debug]
# UNIVERSAL=1 builds Apple Silicon + Intel and merges them with lipo
# (needs: rustup target add aarch64-apple-darwin x86_64-apple-darwin).
set -eu
cd "$(dirname "$0")/.."
profile="${1:-release}"
flag=$([ "$profile" = release ] && echo --release || true)
if [ "${UNIVERSAL:-0}" = 1 ]; then
  for t in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build $flag -p sudopen -p sudopen-finder --target "$t"
  done
  for b in sudopen sudopen-finder; do
    lipo -create "target/aarch64-apple-darwin/$profile/$b" "target/x86_64-apple-darwin/$profile/$b" -output "target/$profile/$b"
  done
else
  cargo build $flag -p sudopen -p sudopen-finder
fi
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
app="target/$profile/sudopen.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "target/$profile/sudopen" "$app/Contents/MacOS/sudopen"
[ -f macos/AppIcon.icns ] || scripts/make-icon.sh >/dev/null
cp macos/AppIcon.icns "$app/Contents/Resources/AppIcon.icns"
# Licenses ship with the app; Credits.html shows in the About window.
cp LICENSE THIRD_PARTY_NOTICES.md macos/Credits.html "$app/Contents/Resources/"
sed "s/__VERSION__/$version/g" macos/Info.plist > "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"

# Finder extension (right-click "sudopen this").
appex="$app/Contents/PlugIns/sudopen-finder.appex"
mkdir -p "$appex/Contents/MacOS"
cp "target/$profile/sudopen-finder" "$appex/Contents/MacOS/sudopen-finder"
sed "s/__VERSION__/$version/g" macos/finder/Info.plist > "$appex/Contents/Info.plist"

# Sign inside out. Ad-hoc here; scripts/dmg.sh re-signs with Developer ID.
codesign --force --sign - --entitlements macos/finder/entitlements.plist "$appex" >/dev/null 2>&1
codesign --force --sign - "$app" >/dev/null 2>&1
echo "$app"
