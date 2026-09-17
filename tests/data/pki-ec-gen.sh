#!/bin/sh
# The ECDSA half of the fixture PKI: what src/neant/crypto/p256.nt is wired into verify.nt and
# tls.nt for. Run from the crate root:  sh tests/data/pki-ec-gen.sh    (OpenSSL 3.6.2)
#
# This is deliberately a SEPARATE script from pki-gen.sh and it writes only pki-ec*.pem / .key.
# pki-gen.sh makes new RSA keys every time it runs, and the RSA-PSS CertificateVerify vector in
# tests/verify.nt is pinned to pki-leaf.key as committed — so the ECDSA fixtures get their own
# script rather than a section that would force that vector to be remade to add a certificate.
#
# The keys are written out and COMMITTED, for the same reason pki-gen.sh commits its own: an
# `openssl s_server` holding an EC certificate is what proves the ecdsa_secp256r1_sha256
# CertificateVerify path really works against a real server, and a throwaway key that only ever
# signs names under .neant.test is worth less than the hermetic test it buys. Never reuse one.
#
# The dates are pinned, not relative, so these stay valid at tests/verify.nt's fixed `vfyNow`.
set -e
cd "$(dirname "$0")"
umask 077
R=20250101000000Z; RE=20450101000000Z          # root validity
L=20250101000000Z; LE=20350101000000Z          # a leaf that is valid at the tests' fixed `now`

CS="/C=US/O=neant fixtures/CN=neant fixture ec root"
CS384="/C=US/O=neant fixtures/CN=neant fixture p384 root"
LS="/C=US/O=neant fixtures/CN=leaf.neant.test"

cat > pki-ec-ext.cnf <<'EOF'
[ca]
basicConstraints = critical,CA:TRUE,pathlen:0
keyUsage         = critical,keyCertSign,cRLSign
[leaf]
basicConstraints = critical,CA:FALSE
keyUsage         = critical,digitalSignature
extendedKeyUsage = serverAuth
subjectAltName   = DNS:leaf.neant.test,IP:127.0.0.1
EOF

# ---- a P-256 root and a P-256 leaf under it. Both signatures are ecdsa-with-SHA256, which is the
# one ECDSA algorithm verify.nt can check, so this chain verifies end to end.
openssl ecparam -name prime256v1 -genkey -noout -out pki-ec-root.key 2>/dev/null
openssl req -x509 -new -key pki-ec-root.key -sha256 -set_serial 41 \
  -not_before $R -not_after $RE -subj "$CS" \
  -addext "basicConstraints=critical,CA:TRUE,pathlen:0" \
  -addext "keyUsage=critical,keyCertSign,cRLSign" -out pki-ec-root.pem 2>/dev/null

openssl ecparam -name prime256v1 -genkey -noout -out pki-ec-leaf.key 2>/dev/null
openssl req -new -key pki-ec-leaf.key -subj "$LS" -out pki-ec-leaf.csr 2>/dev/null
openssl x509 -req -in pki-ec-leaf.csr -CA pki-ec-root.pem -CAkey pki-ec-root.key -set_serial 42 \
  -not_before $L -not_after $LE -sha256 -extfile pki-ec-ext.cnf -extensions leaf \
  -out pki-ec-leaf.pem 2>/dev/null

# ---- the same shape on P-384, signed with SHA-256 so the signature ALGORITHM is the one this
# build knows (ecdsaSha256) and only the CURVE is out of reach. That is the case a verifier gets
# wrong by treating "an EC key" as one thing: the refusal has to name p384.
openssl ecparam -name secp384r1 -genkey -noout -out pki-ec384-root.key 2>/dev/null
openssl req -x509 -new -key pki-ec384-root.key -sha256 -set_serial 43 \
  -not_before $R -not_after $RE -subj "$CS384" \
  -addext "basicConstraints=critical,CA:TRUE,pathlen:0" \
  -addext "keyUsage=critical,keyCertSign,cRLSign" -out pki-ec384-root.pem 2>/dev/null
openssl x509 -req -in pki-ec-leaf.csr -CA pki-ec384-root.pem -CAkey pki-ec384-root.key \
  -set_serial 44 -not_before $L -not_after $LE -sha256 -extfile pki-ec-ext.cnf -extensions leaf \
  -out pki-ec384-leaf.pem 2>/dev/null

rm -f pki-ec-leaf.csr pki-ec-ext.cnf

hdr() {
  f=$1; shift
  { echo "# $*"
    echo "# Regenerate the ECDSA fixtures with:  sh tests/data/pki-ec-gen.sh   (OpenSSL 3.6.2),"
    echo "# which carries the exact command for this file. Dates are pinned there on purpose."
    cat "$f"; } > "$f.h" && mv "$f.h" "$f"
}
hdr pki-ec-root.pem "the fixture ECDSA root: P-256, self-signed with ecdsa-with-SHA256, CA:TRUE pathlen:0"
hdr pki-ec-leaf.pem "a P-256 leaf issued by pki-ec-root with ecdsa-with-SHA256; SAN DNS:leaf.neant.test, IP:127.0.0.1"
hdr pki-ec384-root.pem "a P-384 root: the curve p256.nt does not verify, so a chain to it must be refused BY CURVE"
hdr pki-ec384-leaf.pem "the same leaf issued by the P-384 root, still with ecdsa-with-SHA256: only the curve is out of reach"
for k in pki-ec-root.key pki-ec-leaf.key pki-ec384-root.key; do
  { echo "# A THROWAWAY fixture key, committed on purpose: \`openssl s_server\` needs pki-ec-leaf.key"
    echo "# to complete a real ECDSA handshake in src/main.rs's end-to-end test, and a key that only"
    echo "# ever signs certificates for names under .neant.test buys hermetic tests for nothing."
    echo "# Regenerate with:  sh tests/data/pki-ec-gen.sh"
    cat "$k"; } > "$k.h" && mv "$k.h" "$k"
done
chmod 644 pki-ec*.pem pki-ec*.key
