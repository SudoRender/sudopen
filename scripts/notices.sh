#!/bin/sh
# Regenerates THIRD_PARTY_NOTICES.md: license texts of every Rust crate in
# the app, the bundled Oniguruma C library, and the syntax definitions and
# themes. Run after changing dependencies.
set -eu
cd "$(dirname "$0")/.."
out=THIRD_PARTY_NOTICES.md
{
  printf '# Third-party notices\n\nsudopen is MIT licensed (see LICENSE). It includes the following open source software.\n\n'
  cargo metadata --format-version 1 --filter-platform aarch64-apple-darwin | python3 -c '
import glob, json, os, sys
m = json.load(sys.stdin)
ids = {n["id"] for n in m["resolve"]["nodes"]}
pkgs = sorted((p for p in m["packages"] if p["id"] in ids and not p["name"].startswith("sudopen")), key=lambda p: p["name"])
print("## Rust crates\n")
for p in pkgs:
    print("- {} {} ({})".format(p["name"], p["version"], p.get("license") or "see below"))
print()
seen = {}
for p in pkgs:
    d = os.path.dirname(p["manifest_path"])
    files = sorted(f for f in glob.glob(os.path.join(d, "*")) if os.path.basename(f).upper().startswith(("LICENSE", "LICENCE", "COPYING", "UNLICENSE")))
    for f in files:
        text = open(f, encoding="utf-8", errors="replace").read().strip()
        seen.setdefault(text, []).append("{} {}".format(p["name"], p["version"]))
print("### Crate license texts\n")
for text, users in seen.items():
    print("#### " + ", ".join(users) + "\n")
    print("```\n" + text + "\n```\n")
'
  onig=$(cargo metadata --format-version 1 | python3 -c 'import json,sys; m=json.load(sys.stdin); print(next(p["manifest_path"] for p in m["packages"] if p["name"]=="onig_sys"))')
  printf '## Oniguruma (bundled in onig_sys)\n\n```\n'
  cat "$(dirname "$onig")/oniguruma/COPYING"
  printf '\n```\n\n## Syntax definitions and themes (via bat / two-face)\n\n'
  cargo run -q --release -p sudopen-core --example acknowledgements
} > "$out"
echo "$out: $(wc -l < "$out") lines"
