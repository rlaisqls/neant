#!/bin/sh
# The Ed25519 and SHA-512 half of the fixture PKI: what src/neant/crypto/ed25519.nt and rsa.nt's
# SHA-512 DigestInfo are wired into verify.nt for. Run from the crate root:
#   sh tests/data/pki-ed-gen.sh    (OpenSSL 3.6.2)
#
# A SEPARATE script, for the reason pki-ec-gen.sh gives: pki-gen.sh makes new RSA keys every time
# it runs and tests/verify.nt pins an RSA-PSS CertificateVerify vector to pki-leaf.key as
# committed, so anything that would force that vector to be remade gets its own script instead.
#
# Ed25519 names no digest — RFC 8032 hashes the message internally — so there is no -sha256 here
# and openssl picks nothing: the signature algorithm IS the key type. That is also why verify.nt
# hands ed25519Verify the tbs span rather than a digest of it.
#
# The dates are pinned, not relative, so these stay valid at tests/verify.nt's fixed `vfyNow`.
set -e
cd "$(dirname "$0")"
umask 077
R=20250101000000Z; RE=20450101000000Z          # root validity
L=20250101000000Z; LE=20350101000000Z          # a leaf that is valid at the tests' fixed `now`

EDS="/C=US/O=neant fixtures/CN=neant fixture ed25519 root"
R5S="/C=US/O=neant fixtures/CN=neant fixture sha512 root"
LS="/C=US/O=neant fixtures/CN=leaf.neant.test"

cat > pki-ed-ext.cnf <<'EOF'
[leaf]
basicConstraints = critical,CA:FALSE
keyUsage         = critical,digitalSignature
extendedKeyUsage = serverAuth
subjectAltName   = DNS:leaf.neant.test,IP:127.0.0.1
EOF

# ---- an Ed25519 root and an Ed25519 leaf under it. Both signatures are id-Ed25519 (1.3.101.112),
# which verify.nt can check now that tlsclient.nt loads ed25519.nt, so this chain verifies.
openssl genpkey -algorithm ed25519 -out pki-ed-root.key 2>/dev/null
openssl req -x509 -new -key pki-ed-root.key -set_serial 51 \
  -not_before $R -not_after $RE -subj "$EDS" \
  -addext "basicConstraints=critical,CA:TRUE,pathlen:0" \
  -addext "keyUsage=critical,keyCertSign,cRLSign" -out pki-ed-root.pem 2>/dev/null

openssl genpkey -algorithm ed25519 -out pki-ed-leaf.key 2>/dev/null
openssl req -new -key pki-ed-leaf.key -subj "$LS" -out pki-ed-leaf.csr 2>/dev/null
openssl x509 -req -in pki-ed-leaf.csr -CA pki-ed-root.pem -CAkey pki-ed-root.key -set_serial 52 \
  -not_before $L -not_after $LE -extfile pki-ed-ext.cnf -extensions leaf \
  -out pki-ed-leaf.pem 2>/dev/null

# ---- RSA-2048 signed with sha512WithRSAEncryption, the third entry in rsa.nt's DigestInfo table.
openssl genrsa -out pki-sha512-root.key 2048 2>/dev/null
openssl req -x509 -new -key pki-sha512-root.key -sha512 -set_serial 53 \
  -not_before $R -not_after $RE -subj "$R5S" \
  -addext "basicConstraints=critical,CA:TRUE,pathlen:0" \
  -addext "keyUsage=critical,keyCertSign,cRLSign" -out pki-sha512-root.pem 2>/dev/null

openssl genrsa -out pki-sha512-leaf.key 2048 2>/dev/null
openssl req -new -key pki-sha512-leaf.key -subj "$LS" -out pki-sha512-leaf.csr 2>/dev/null
openssl x509 -req -in pki-sha512-leaf.csr -CA pki-sha512-root.pem -CAkey pki-sha512-root.key \
  -set_serial 54 -not_before $L -not_after $LE -sha512 -extfile pki-ed-ext.cnf -extensions leaf \
  -out pki-sha512-leaf.pem 2>/dev/null

# ---- and the PSS form of the same, so rsaPssSha512 is exercised and not merely listed.
openssl x509 -req -in pki-sha512-leaf.csr -CA pki-sha512-root.pem -CAkey pki-sha512-root.key \
  -set_serial 55 -not_before $L -not_after $LE -sha512 \
  -sigopt rsa_padding_mode:pss -sigopt rsa_pss_saltlen:64 -sigopt rsa_mgf1_md:sha512 \
  -extfile pki-ed-ext.cnf -extensions leaf -out pki-pss512-leaf.pem 2>/dev/null

# ---- extendedKeyUsage, which verify.nt enforces for serverAuth and nests through intermediates.
# Three certificates that differ ONLY in their EKU, so a refusal can be attributed to it and to
# nothing else: one issued for S/MIME, one with no EKU at all (absent is unconstrained, RFC 5280
# 4.2.1.12), and an intermediate restricted to S/MIME issuing a perfectly good server leaf.
cat > pki-eku-ext.cnf <<'EOF'
[email]
basicConstraints = critical,CA:FALSE
keyUsage         = critical,digitalSignature
extendedKeyUsage = emailProtection
subjectAltName   = DNS:leaf.neant.test,IP:127.0.0.1
[noeku]
basicConstraints = critical,CA:FALSE
keyUsage         = critical,digitalSignature
subjectAltName   = DNS:leaf.neant.test,IP:127.0.0.1
[emailca]
basicConstraints = critical,CA:TRUE,pathlen:0
keyUsage         = critical,keyCertSign,cRLSign
extendedKeyUsage = emailProtection
[server]
basicConstraints = critical,CA:FALSE
keyUsage         = critical,digitalSignature
extendedKeyUsage = serverAuth
subjectAltName   = DNS:leaf.neant.test,IP:127.0.0.1
EOF

openssl req -new -key pki-sha512-leaf.key -subj "$LS" -out pki-eku.csr 2>/dev/null
openssl x509 -req -in pki-eku.csr -CA pki-sha512-root.pem -CAkey pki-sha512-root.key -set_serial 56   -not_before $L -not_after $LE -sha256 -extfile pki-eku-ext.cnf -extensions email   -out pki-eku-email-leaf.pem 2>/dev/null
openssl x509 -req -in pki-eku.csr -CA pki-sha512-root.pem -CAkey pki-sha512-root.key -set_serial 57   -not_before $L -not_after $LE -sha256 -extfile pki-eku-ext.cnf -extensions noeku   -out pki-eku-none-leaf.pem 2>/dev/null

# an intermediate that may only do S/MIME, and a leaf under it that asks for serverAuth
openssl genrsa -out pki-eku-int.key 2048 2>/dev/null
openssl req -new -key pki-eku-int.key -subj "/C=US/O=neant fixtures/CN=neant fixture s-mime only ca" -out pki-eku-int.csr 2>/dev/null
openssl x509 -req -in pki-eku-int.csr -CA pki-sha512-root.pem -CAkey pki-sha512-root.key -set_serial 58   -not_before $R -not_after $RE -sha256 -extfile pki-eku-ext.cnf -extensions emailca   -out pki-eku-int.pem 2>/dev/null
openssl x509 -req -in pki-eku.csr -CA pki-eku-int.pem -CAkey pki-eku-int.key -set_serial 59   -not_before $L -not_after $LE -sha256 -extfile pki-eku-ext.cnf -extensions server   -out pki-eku-int-leaf.pem 2>/dev/null

rm -f pki-ed-leaf.csr pki-sha512-leaf.csr pki-ed-ext.cnf pki-eku.csr pki-eku-int.csr pki-eku-ext.cnf

hdr() {
  f=$1; shift
  { echo "# $*"
    echo "# Regenerate with:  sh tests/data/pki-ed-gen.sh   (OpenSSL 3.6.2), which carries the exact"
    echo "# command for this file. Dates are pinned there on purpose."
    cat "$f"; } > "$f.h" && mv "$f.h" "$f"
}
hdr pki-ed-root.pem "the fixture Ed25519 root: self-signed with id-Ed25519, CA:TRUE pathlen:0"
hdr pki-ed-leaf.pem "an Ed25519 leaf issued by pki-ed-root; SAN DNS:leaf.neant.test, IP:127.0.0.1"
hdr pki-sha512-root.pem "an RSA-2048 root self-signed with sha512WithRSAEncryption"
hdr pki-sha512-leaf.pem "an RSA leaf issued by it with sha512WithRSAEncryption"
hdr pki-pss512-leaf.pem "the same leaf signed RSASSA-PSS with SHA-512 and a 64-byte salt"
hdr pki-eku-email-leaf.pem "a leaf issued for emailProtection: refused for TLS by extendedKeyUsage, and by nothing else"
hdr pki-eku-none-leaf.pem "the same leaf with NO extendedKeyUsage: unconstrained, so it must be accepted"
hdr pki-eku-int.pem "an intermediate restricted to emailProtection: nothing under it may serve TLS"
hdr pki-eku-int-leaf.pem "a serverAuth leaf under that intermediate: refused because the constraint nests"
for k in pki-ed-root.key pki-ed-leaf.key pki-sha512-root.key pki-sha512-leaf.key pki-eku-int.key; do
  { echo "# A THROWAWAY fixture key, committed on purpose, as pki-gen.sh's are: a key that only ever"
    echo "# signs certificates for names under .neant.test buys hermetic tests for nothing."
    echo "# Regenerate with:  sh tests/data/pki-ed-gen.sh"
    cat "$k"; } > "$k.h" && mv "$k.h" "$k"
done
chmod 644 pki-ed*.pem pki-ed*.key pki-sha512*.pem pki-sha512*.key pki-pss512*.pem pki-eku*.pem pki-eku*.key
