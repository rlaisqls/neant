#!/bin/sh
# Run IOLB (Olivry et al. 2020, https://gitlab.inria.fr/CORSE/iolb) on one exported SCoP, inside
# its docker image, and print its raw output. `neant cost --iolb` calls this through
# `NEANT_IOLB="tests/kernels/iolb.sh {file}"` and reads the second-to-last line.
#
# Needs a built IOLB checkout in $IOLB_DIR (`make` inside the image builds `iolb-affine`) and, on a
# non-x86 host, amd64 emulation for docker (`docker run --privileged --rm tonistiigi/binfmt --install amd64`).
set -e
[ -n "$IOLB_DIR" ] || { echo "IOLB_DIR is not set (an IOLB checkout with iolb-affine built)" >&2; exit 2; }
src=$(realpath "$1")
tmp=$(mktemp -d "$IOLB_DIR/neant-XXXXXX")
cp "$src" "$tmp/scop.c"
# IOLB's search is exponential in the worst case; a nest it cannot finish in $IOLB_TIMEOUT seconds
# (default 120) yields no bound rather than a hang
name=neant-iolb-$$
cleanup() { st=$?; docker kill "$name" >/dev/null 2>&1 || true; rm -rf "$tmp"; exit $st; }
trap cleanup EXIT
timeout -s KILL "${IOLB_TIMEOUT:-120}" docker run --rm --name "$name" --platform linux/amd64 \
  -v "$IOLB_DIR":/hst -w /hst registry.gitlab.inria.fr/corse/iolb ./iolb-affine "$(basename "$tmp")/scop.c" >"$tmp/out" 2>/dev/null \
  && cat "$tmp/out"
