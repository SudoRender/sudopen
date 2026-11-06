#!/bin/sh
# Builds the release app, copies it to /Applications and links the
# `sudopen` command into a bin directory on PATH.
# Usage: scripts/install.sh [bin dir]   (default: /opt/homebrew/bin if it exists, else /usr/local/bin)
set -eu
cd "$(dirname "$0")/.."
app=$(scripts/bundle.sh release | tail -1)
"$app/Contents/MacOS/sudopen" --quit 2>/dev/null || true
rm -rf /Applications/sudopen.app
cp -R "$app" /Applications/sudopen.app
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f /Applications/sudopen.app
# Refresh the Services registry so "Open in sudopen" shows up in Finder.
/System/Library/CoreServices/pbs -update
bin="${1:-}"
if [ -z "$bin" ]; then
  if [ -d /opt/homebrew/bin ] && [ -w /opt/homebrew/bin ]; then bin=/opt/homebrew/bin; else bin=/usr/local/bin; fi
fi
target=/Applications/sudopen.app/Contents/MacOS/sudopen
if [ -w "$bin" ]; then ln -sf "$target" "$bin/sudopen"; else sudo ln -sf "$target" "$bin/sudopen"; fi
echo "installed /Applications/sudopen.app and $bin/sudopen"
