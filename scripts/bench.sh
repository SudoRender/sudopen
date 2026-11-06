#!/bin/sh
# Prints open-to-first-frame times: cold start, warm opens, big files.
# Test files are generated once in $TMPDIR/sudopen-bench.
# The app is restarted, so any open sudopen windows close.
set -eu
cd "$(dirname "$0")/.."
bin=target/release/sudopen.app/Contents/MacOS/sudopen
scripts/bundle.sh release >/dev/null
dir="${TMPDIR:-/tmp}/sudopen-bench"
mkdir -p "$dir"
dir=$(cd "$dir" && pwd -P)
[ -f "$dir/big.log" ] || seq -f '%.0f 2026-11-05T23:00:00.000Z INFO  worker request path=/api/v1/items status=200 dur=12ms' 1 15000000 > "$dir/big.log"
[ -f "$dir/one-line.json" ] || { printf '['; seq -f '{"id":%.0f,"name":"item","tags":["a","b"]},' 1 2500000 | tr -d '\n'; printf '{}]'; } > "$dir/one-line.json"
[ -f "$dir/big.rs" ] || for i in $(seq 1 700); do cat crates/app/src/view.rs; done > "$dir/big.rs"

run() { "$bin" --timing "$@" | sed "s|$dir/||"; }

"$bin" --quit
sleep 1
echo "== cold start (app not running)"
cp crates/app/src/view.rs "$dir/cold.rs"
run "$dir/cold.rs"
echo "== warm opens"
for i in 1 2 3 4 5; do cp crates/app/src/view.rs "$dir/warm$i.rs"; run "$dir/warm$i.rs"; done
echo "== big files ($(du -h "$dir/big.log" | cut -f1) log, $(du -h "$dir/one-line.json" | cut -f1) one-line json, $(du -h "$dir/big.rs" | cut -f1) rust)"
run "$dir/big.log"
run "$dir/one-line.json"
run "$dir/big.rs"
echo "== jump into an open file"
run "$dir/big.rs:150000"
