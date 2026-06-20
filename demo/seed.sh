#!/usr/bin/env bash
#
# Seed a demo transfs store with a handful of varied, tagged documents so you can
# try the facets-default query-path mount. Generates throwaway sample files,
# archives them, tags them, and prints how to mount and browse.
#
# Usage:
#   demo/seed.sh [store-dir]      # default store: demo/store (gitignored)
#
# The store it creates is NOT committed — it's a rebuildable cache of your tags +
# content-addressed blobs. Re-run any time; it starts fresh.

set -euo pipefail

# --- locate the repo + binary (build if needed) ---
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$here"
bin="bin/transfs2"
if [[ ! -x "$bin" ]]; then
  echo "building $bin ..."
  crystal build src/cli2.cr -o "$bin"
fi

store="${1:-demo/store}"
rm -rf "$store"
mkdir -p "$store"

t() { "$bin" --store "$store" "$@"; }   # shorthand

# --- generate throwaway sample files (type is derived from the extension) ---
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
printf 'Q1 2024 financials\n'      > "$tmp/q1-report.pdf"
printf 'Q2 2024 financials\n'      > "$tmp/q2-report.pdf"
printf 'Globex invoice #1042\n'    > "$tmp/invoice.pdf"
printf 'fake jpeg bytes\n'         > "$tmp/beach.jpg"
printf 'fake png bytes\n'          > "$tmp/sunset.png"
printf '# Project notes\n'         > "$tmp/notes.md"
printf 'pasta, garlic, olive oil\n'> "$tmp/recipe.txt"

# add <file> [name] -> capture the short id printed as the 2nd word of the output
add() { t add "$1" "$(basename "$1")" | awk '{print $2}'; }

# archive + tag (key=value tags need the `--` literal-positional guard)
id=$(add "$tmp/q1-report.pdf"); t tag "$id" -- project=acme finance stars=4 date=2024/01/31
id=$(add "$tmp/q2-report.pdf"); t tag "$id" -- project=acme finance stars=5 date=2024/04/30
id=$(add "$tmp/invoice.pdf");   t tag "$id" -- project=globex finance stars=3 date=2023/11/02
id=$(add "$tmp/beach.jpg");     t tag "$id" -- vacation personal year=2023
id=$(add "$tmp/sunset.png");    t tag "$id" -- vacation stars=5 year=2024
id=$(add "$tmp/notes.md");      t tag "$id" -- work project=acme
id=$(add "$tmp/recipe.txt");    t tag "$id" -- personal cooking

echo
echo "seeded $(t list | wc -l) documents into: $store"
echo
echo "browse it:"
echo "  mkdir -p demo/mnt"
echo "  $bin --store $store mount demo/mnt &      # read-only, runs in background"
echo "  ls demo/mnt/                  # the facet menu: project/ year/ stars/ tag/ type/ date/"
echo "  ls demo/mnt/project/acme/=/   # the documents tagged project=acme"
echo "  ls demo/mnt/date/2024/        # walk the date hierarchy"
echo "  ls demo/mnt/=/                # everything, newest first"
echo "  cat demo/mnt/tag/finance/=/q1-report.pdf"
echo "  fusermount3 -u demo/mnt       # unmount when done"
