#!/bin/sh
# Regenerates macos/AppIcon.icns from macos/icon/icon.swift.
# Usage: scripts/make-icon.sh [variant]   (default: root-page)
# Needs only what ships with macOS + the command line tools: swiftc, iconutil.
set -eu
cd "$(dirname "$0")/.."
variant="${1:-root-page}"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/sudopen-icon.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
swiftc -O macos/icon/icon.swift -o "$tmp/icon"
"$tmp/icon" iconset "$tmp/AppIcon.iconset" "$variant"
iconutil -c icns "$tmp/AppIcon.iconset" -o macos/AppIcon.icns
echo macos/AppIcon.icns
