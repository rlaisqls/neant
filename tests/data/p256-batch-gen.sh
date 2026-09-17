#!/bin/sh
# The batch corpus tests/crypto/batch.nt verifies: 512 real ECDSA-with-SHA-256 signatures over four
# P-256 keys, which is what src/neant/crypto/batch.nt's batch-of-512 assertions and its throughput
# table run on. Regenerate with  sh tests/data/p256-batch-gen.sh > tests/data/p256-batch.txt
# (OpenSSL 3.6.2). ECDSA signing picks a random k, so a re-run gives different (r,s) pairs that
# verify just as well; the committed file is the one this machine produced.
#
# Format, one token per column, hex throughout:
#   K <uncompressed SEC 1 public key>          four of them, first
#   S <key index> <SHA-256 digest> <DER signature>
set -e
d=$(mktemp -d)
trap 'rm -rf "$d"' 0
i=0
while [ $i -lt 4 ]; do
  openssl ecparam -name prime256v1 -genkey -noout -out "$d/k$i.pem" 2>/dev/null
  echo "K $(openssl ec -in "$d/k$i.pem" -pubout -outform DER 2>/dev/null | tail -c 65 | xxd -p -c 200)"
  i=$((i+1))
done
i=0
while [ $i -lt 512 ]; do
  k=$((i % 4))
  printf 'neant p256 batch vector %d' "$i" > "$d/m.bin"
  dg=$(openssl dgst -sha256 -binary "$d/m.bin" | xxd -p -c 200)
  sg=$(openssl dgst -sha256 -sign "$d/k$k.pem" "$d/m.bin" | xxd -p -c 400)
  echo "S $k $dg $sg"
  i=$((i+1))
done
