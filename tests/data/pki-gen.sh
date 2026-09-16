#!/bin/sh
# The fixture PKI src/main.rs's `mod verify` chain cases and its end-to-end handshake run against.
# Run from the crate root:  sh tests/data/pki-gen.sh    (OpenSSL 3.6.2)
#
# Everything here is deliberately reproducible and deliberately throwaway. The keys are written to
# tests/data/pki-*.key and COMMITTED: pki-leaf.key is what `openssl s_server` needs to complete a
# real handshake in the end-to-end test, and a key that only ever signs fixture certificates for
# names under .neant.test is worth less than the hermeticity it buys. Never reuse one anywhere.
#
# The dates are fixed rather than relative because the expiry cases are the point: pki-expired.pem
# must stay expired and pki-leaf.pem must stay valid, whenever the tests are run.
#
# Re-running this makes new keys, so the RSA-PSS CertificateVerify vector in src/main.rs -- SIG in
# mod verify's certificate_verify_matches_rfc8446_and_openssl -- has to be remade at the same time;
# that test's own comment carries the two commands that produce it from pki-leaf.key.
set -e
cd "$(dirname "$0")"
umask 077
R=20250101000000Z; RE=20450101000000Z          # root and intermediate validity
L=20250101000000Z; LE=20350101000000Z          # a leaf that is valid now

k() { openssl genrsa -out "$1" 2048 2>/dev/null; }
# sign[csr; issuer cert; issuer key; serial; notBefore; notAfter; ext section; out]
sign() {
  openssl x509 -req -in "$1" -CA "$2" -CAkey "$3" -set_serial "$4" \
    -not_before "$5" -not_after "$6" -sha256 -extfile pki-ext.cnf -extensions "$7" \
    -out "$8" 2>/dev/null
}

cat > pki-ext.cnf <<'EOF'
[ca1]
basicConstraints = critical,CA:TRUE,pathlen:1
keyUsage         = critical,keyCertSign,cRLSign
[ca0]
basicConstraints = critical,CA:TRUE,pathlen:0
keyUsage         = critical,keyCertSign,cRLSign
[notca]
basicConstraints = critical,CA:FALSE
keyUsage         = critical,digitalSignature,keyCertSign
[leaf]
basicConstraints = critical,CA:FALSE
keyUsage         = critical,digitalSignature,keyEncipherment
extendedKeyUsage = serverAuth
subjectAltName   = DNS:leaf.neant.test,IP:127.0.0.1
[other]
basicConstraints = critical,CA:FALSE
keyUsage         = critical,digitalSignature,keyEncipherment
extendedKeyUsage = serverAuth
subjectAltName   = DNS:other.neant.test
[crit]
basicConstraints = critical,CA:FALSE
subjectAltName   = DNS:leaf.neant.test
1.3.6.1.4.1.99999.1 = critical,DER:05:00
[wild]
basicConstraints = critical,CA:FALSE
keyUsage         = critical,digitalSignature,keyEncipherment
extendedKeyUsage = serverAuth
subjectAltName   = DNS:*.wild.neant.test,DNS:host.two.neant.test
EOF

# ---- the root, self-signed, pathlen:1 so it may sit above exactly one intermediate
k pki-root.key
openssl req -x509 -new -key pki-root.key -sha256 -set_serial 1 -not_before $R -not_after $RE \
  -subj "/C=US/O=neant fixtures/CN=neant fixture root" \
  -addext "basicConstraints=critical,CA:TRUE,pathlen:1" \
  -addext "keyUsage=critical,keyCertSign,cRLSign" -out pki-root.pem 2>/dev/null

# ---- the intermediate, pathlen:0
k pki-int.key
openssl req -new -key pki-int.key -subj "/C=US/O=neant fixtures/CN=neant fixture intermediate" \
  -out pki-int.csr 2>/dev/null
sign pki-int.csr pki-root.pem pki-root.key 2 $R $RE ca0 pki-int.pem

# ---- an "intermediate" with CA:FALSE, and a leaf under it. The signature chains, the names chain,
# only basicConstraints says no -- the single check that stops any leaf holder minting certificates.
k pki-notca.key
openssl req -new -key pki-notca.key -subj "/C=US/O=neant fixtures/CN=neant fixture not-a-ca" \
  -out pki-notca.csr 2>/dev/null
sign pki-notca.csr pki-root.pem pki-root.key 3 $R $RE notca pki-notca.pem

# ---- the leaves, all signed by the intermediate unless said otherwise
k pki-leaf.key
LS="/C=US/O=neant fixtures/CN=leaf.neant.test"
openssl req -new -key pki-leaf.key -subj "$LS" -out pki-leaf.csr 2>/dev/null
sign pki-leaf.csr pki-int.pem pki-int.key 16 $L $LE leaf pki-leaf.pem
sign pki-leaf.csr pki-int.pem pki-int.key 17 20200101000000Z 20210101000000Z leaf pki-expired.pem
sign pki-leaf.csr pki-int.pem pki-int.key 18 20350101000000Z 20360101000000Z leaf pki-future.pem
sign pki-leaf.csr pki-int.pem pki-int.key 19 $L $LE other pki-wrong-host.pem
sign pki-leaf.csr pki-int.pem pki-int.key 20 $L $LE wild pki-wildcard.pem
sign pki-leaf.csr pki-int.pem pki-int.key 21 $L $LE crit pki-critical.pem
sign pki-leaf.csr pki-notca.pem pki-notca.key 22 $L $LE leaf pki-under-notca.pem

# ---- a second CA under the pathlen:0 intermediate, and a leaf under that. Every signature is
# good and every name chains; only pki-int.pem's pathLenConstraint says the path is too long.
k pki-int2.key
openssl req -new -key pki-int2.key -subj "/C=US/O=neant fixtures/CN=neant fixture intermediate 2" \
  -out pki-int2.csr 2>/dev/null
sign pki-int2.csr pki-int.pem pki-int.key 4 $R $RE ca0 pki-int2.pem
sign pki-leaf.csr pki-int2.pem pki-int2.key 23 $L $LE leaf pki-deep.pem

# ---- a leaf that signed itself: the names chain to nothing and no trust store holds it
k pki-ss.key
openssl req -x509 -new -key pki-ss.key -sha256 -set_serial 32 -not_before $L -not_after $LE \
  -subj "$LS" -addext "basicConstraints=critical,CA:FALSE" \
  -addext "subjectAltName=DNS:leaf.neant.test,IP:127.0.0.1" -out pki-selfsigned.pem 2>/dev/null

rm -f pki-int.csr pki-int2.csr pki-leaf.csr pki-notca.csr pki-ext.cnf

# ---- a one-line header on every file saying what it is, the way the older fixtures carry theirs.
# pemLoad ignores anything outside the BEGIN/END lines and so does OpenSSL, so these are free.
hdr() {
  f=$1; shift
  { echo "# $*"
    echo "# Regenerate the whole fixture PKI with:  sh tests/data/pki-gen.sh   (OpenSSL 3.6.2),"
    echo "# which carries the exact command for this file. Dates are pinned there on purpose."
    cat "$f"; } > "$f.h" && mv "$f.h" "$f"
}
hdr pki-root.pem "the fixture root CA: self-signed, CA:TRUE pathlen:1, valid 2025-2045"
hdr pki-int.pem "the fixture intermediate: CA:TRUE pathlen:0, issued by pki-root"
hdr pki-int2.pem "a second CA under the pathlen:0 intermediate, so a chain through it is one link too deep"
hdr pki-notca.pem "an \"intermediate\" with CA:FALSE: every signature and every name chains, only basicConstraints says no"
hdr pki-leaf.pem "the good leaf: SAN DNS:leaf.neant.test and IP:127.0.0.1, issued by pki-int, valid 2025-2035"
hdr pki-expired.pem "the same leaf, valid 2020-2021: expired"
hdr pki-future.pem "the same leaf, valid 2035-2036: not valid yet"
hdr pki-wrong-host.pem "the same leaf with SAN DNS:other.neant.test, so only the hostname is wrong"
hdr pki-wildcard.pem "SAN DNS:*.wild.neant.test and DNS:host.two.neant.test"
hdr pki-critical.pem "a leaf carrying a critical 1.3.6.1.4.1.99999.1 nothing models"
hdr pki-under-notca.pem "a leaf issued by pki-notca"
hdr pki-deep.pem "a leaf issued by pki-int2"
hdr pki-selfsigned.pem "a leaf that signed itself, with the good SAN: it chains to nothing"
for k in pki-*.key; do
  { echo "# A THROWAWAY fixture key, committed on purpose: \`openssl s_server\` needs pki-leaf.key to"
    echo "# complete a real handshake in src/main.rs's end-to-end test, and a key that only ever signs"
    echo "# certificates for names under .neant.test buys hermetic tests for nothing. Never reuse it."
    echo "# Regenerate with:  sh tests/data/pki-gen.sh"
    cat "$k"; } > "$k.h" && mv "$k.h" "$k"
done
chmod 644 pki-*.pem pki-*.key
