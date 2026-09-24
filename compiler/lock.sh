#!/bin/sh
# Regenerates `compiler/costs.lock`, the compiler's own cost report, from the same concatenation
# `build.sh` compiles; with `--check`, diffs it instead and fails on any difference. The tier
# counts it prints are stage D's tracked number (docs/plan.md § Stage D (4)).
set -e
ROOT=$(cd "$(dirname "$0")/.." && pwd)
NEANT=${NEANT:-$ROOT/bootstrap/target/release/neant}
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

cat "$ROOT"/compiler/lex.nt "$ROOT"/compiler/parse.nt "$ROOT"/compiler/check.nt \
    "$ROOT"/compiler/emit.nt "$ROOT"/compiler/poly.nt "$ROOT"/compiler/cost.nt \
    "$ROOT"/compiler/main.nt > "$TMP/all.nt"
[ -f "$ROOT/compiler/costs.lock" ] && cp "$ROOT/compiler/costs.lock" "$TMP/costs.lock"

if [ "$1" = "--check" ]; then
    "$NEANT" lock "$TMP/all.nt" --check
else
    "$NEANT" lock "$TMP/all.nt"
    cp "$TMP/costs.lock" "$ROOT/compiler/costs.lock"
fi
# the tier is the last word of a line, or the word before `rests on`; an unknown says so up front
grep -v '^#' "$ROOT/compiler/costs.lock" | grep -v '^layout ' | sed 's/  rests on .*//' | awk '
    $2 == "unknown:" { t["unknown"]++; n++; next }
    { t[$NF]++; n++ }
    END { printf "%d exact, %d modulo, %d bound, %d unknown, of %d\n", t["exact"], t["modulo"], t["bound"], t["unknown"], n }'
