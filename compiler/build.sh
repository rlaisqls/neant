#!/bin/sh
# Builds seed two: `neant.c`, the self-hosted compiler compiled by itself, and checks that it has
# reached its fixpoint. Run from the repo root, with the Rust compiler already built.
#
# The concatenation is the whole build system. There is no module system, so `compiler/*.nt` are
# fragments of one program and the order is the dependency order: `parse.nt` uses `lex.nt`'s
# `Token`, `check.nt` uses both, and so on. Recorded here rather than in a comment in each file.
set -e
ROOT=$(cd "$(dirname "$0")/.." && pwd)
NEANT=${NEANT:-$ROOT/bootstrap/target/debug/neant}
CC=${CC:-cc}
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

cat "$ROOT"/compiler/lex.nt "$ROOT"/compiler/parse.nt "$ROOT"/compiler/check.nt \
    "$ROOT"/compiler/emit.nt "$ROOT"/compiler/poly.nt "$ROOT"/compiler/cost.nt \
    "$ROOT"/compiler/main.nt > "$TMP/all.nt"

# stage 1: the Rust compiler interprets the neant compiler, which compiles its own source
"$NEANT" run "$TMP/all.nt" < "$TMP/all.nt" > "$TMP/stage2.c"
$CC -O2 -std=gnu11 -w -o "$TMP/stage2" "$TMP/stage2.c" "$ROOT/bootstrap/rt.c"

# stage 2: the same compiler, native, compiles the same source — and must write the same bytes
"$TMP/stage2" < "$TMP/all.nt" > "$TMP/stage3.c"
cmp "$TMP/stage2.c" "$TMP/stage3.c"

cp "$TMP/stage2.c" "$ROOT/bootstrap/neant.c"
echo "bootstrap/neant.c: $(wc -c < "$ROOT/bootstrap/neant.c") bytes, fixpoint reached"
