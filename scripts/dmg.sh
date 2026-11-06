#!/bin/sh
# Builds the installer: target/dist/sudopen-<version>.dmg
#
# The app and the DMG are signed with the Developer ID Application identity
# from the keychain (override with SIGN_ID), with the hardened runtime.
# The DMG is then notarized and stapled, with NOTARY_APPLE_ID, NOTARY_TEAM_ID
# and NOTARY_PASSWORD if set (CI), otherwise with the notarytool keychain
# profile NOTARY_PROFILE (default "gabgren"). NOTARIZE=0 skips it.
# dmgbuild is installed once into target/dmgbuild-venv.
set -eu
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
app=$(scripts/bundle.sh release | tail -1)
out="target/dist/sudopen-$version.dmg"
work=target/dmg

sign_id="${SIGN_ID:-$(security find-identity -v -p codesigning | sed -n 's/.*"\(Developer ID Application:[^"]*\)".*/\1/p' | head -1)}"
if [ -n "$sign_id" ]; then
  codesign --force --options runtime --timestamp --entitlements macos/finder/entitlements.plist \
    --sign "$sign_id" "$app/Contents/PlugIns/sudopen-finder.appex"
  codesign --force --options runtime --timestamp --sign "$sign_id" "$app"
  codesign --verify --strict --deep "$app"
else
  echo "warning: no Developer ID identity; the app stays ad-hoc signed" >&2
fi

rm -rf "$work" && mkdir -p "$work" target/dist
swiftc -O macos/dmg/background.swift -o "$work/background"
"$work/background" "$work/bg.png" 1
"$work/background" "$work/bg@2x.png" 2
tiffutil -cathidpicheck "$work/bg.png" "$work/bg@2x.png" -out "$work/background.tiff" >/dev/null

venv=target/dmgbuild-venv
if [ ! -x "$venv/bin/dmgbuild" ]; then
  python3 -m venv "$venv"
  "$venv/bin/pip" install --quiet --disable-pip-version-check dmgbuild
fi
rm -f "$out"
"$venv/bin/dmgbuild" -s macos/dmg/settings.py \
  -D app="$app" -D background="$work/background.tiff" -D icon=macos/AppIcon.icns \
  "sudopen" "$out" >/dev/null

if [ -n "$sign_id" ]; then
  codesign --force --timestamp --identifier com.sudorender.sudopen.dmg --sign "$sign_id" "$out"
fi

profile="${NOTARY_PROFILE:-gabgren}"
if [ "${NOTARIZE:-1}" = 1 ] && [ -n "$sign_id" ]; then
  submitted=1
  if [ -n "${NOTARY_APPLE_ID:-}" ]; then
    xcrun notarytool submit "$out" --apple-id "$NOTARY_APPLE_ID" --team-id "$NOTARY_TEAM_ID" \
      --password "$NOTARY_PASSWORD" --wait --timeout 30m
  elif xcrun notarytool history --keychain-profile "$profile" >/dev/null 2>&1; then
    xcrun notarytool submit "$out" --keychain-profile "$profile" --wait --timeout 30m
  else
    submitted=0
  fi
  if [ "$submitted" = 1 ]; then
    xcrun stapler staple "$out"
    spctl --assess --type open --context context:primary-signature -v "$out"
  else
    echo "notarization skipped: no notarytool profile \"$profile\" in this keychain." >&2
    echo "  create it once with: xcrun notarytool store-credentials $profile" >&2
  fi
fi
echo "$out"
