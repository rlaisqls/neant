#!/bin/sh
# The SHA-384 half of the fixture PKI: what src/neant/crypto/{sha512,p384}.nt are wired into
# verify.nt and tls.nt for. Run from the crate root:  sh tests/data/pki-p384-gen.sh  (OpenSSL 3.6.2)
#
# A THIRD script, for the same reason pki-ec-gen.sh is separate from pki-gen.sh: re-running a
# generator that makes new keys invalidates every signature vector pinned against the old ones.
# pki-gen.sh owns the RSA keys (and the RSA-PSS CertificateVerify vector hangs off pki-leaf.key),
# pki-ec-gen.sh owns the P-256 keys (and tests/p256.nt's vector hangs off pki-ec-leaf.key), and
# this one owns only pki-p384-*. It READS pki-int.key, pki-ec-leaf.key and pki-ec-root.key as they
# are committed and never writes them, so running it does not disturb either of the other two.
#
# The keys it does write are committed, like the others: `openssl s_server` holding a P-384
# certificate is what proves the ecdsa_secp384r1_sha384 CertificateVerify path works against a real
# server, and a key that only ever signs names under .neant.test is worth less than that test.
#
# The dates are pinned, not relative, so these stay valid at tests/verify.nt's fixed `vfyNow`.
set -e
cd "$(dirname "$0")"
umask 077
R=20250101000000Z; RE=20450101000000Z          # root validity
L=20250101000000Z; LE=20350101000000Z          # a leaf that is valid at the tests' fixed `now`

RS="/C=US/O=neant fixtures/CN=neant fixture p384 ca"
LS="/C=US/O=neant fixtures/CN=leaf.neant.test"

cat > pki-p384-ext.cnf <<'EOF'
[leaf]
basicConstraints = critical,CA:FALSE
keyUsage         = critical,digitalSignature
extendedKeyUsage = serverAuth
subjectAltName   = DNS:leaf.neant.test,IP:127.0.0.1
EOF

# ---- the chain this build can now check end to end: a P-384 root and a P-384 leaf under it, both
# signed ecdsa-with-SHA384. Before sha512.nt and p384.nt neither link could be verified at all.
openssl ecparam -name secp384r1 -genkey -noout -out pki-p384-root.key 2>/dev/null
openssl req -x509 -new -key pki-p384-root.key -sha384 -set_serial 51 \
  -not_before $R -not_after $RE -subj "$RS" \
  -addext "basicConstraints=critical,CA:TRUE,pathlen:0" \
  -addext "keyUsage=critical,keyCertSign,cRLSign" -out pki-p384-root.pem 2>/dev/null

openssl ecparam -name secp384r1 -genkey -noout -out pki-p384-leaf.key 2>/dev/null
openssl req -new -key pki-p384-leaf.key -subj "$LS" -sha384 -out pki-p384-leaf.csr 2>/dev/null
openssl x509 -req -in pki-p384-leaf.csr -CA pki-p384-root.pem -CAkey pki-p384-root.key \
  -set_serial 52 -not_before $L -not_after $LE -sha384 \
  -extfile pki-p384-ext.cnf -extensions leaf -out pki-p384-leaf.pem 2>/dev/null

# ---- the two RSA-with-SHA-384 leaves, under the RSA intermediate pki-gen.sh already made, so that
# rsa.nt's SHA-384 DigestInfo and its MGF1-SHA384 each get a real certificate rather than a
# hand-built block. Same subject and same key as pki-leaf: only the signature algorithm differs.
openssl req -new -key pki-leaf.key -subj "$LS" -sha384 -out pki-sha384-leaf.csr 2>/dev/null
openssl x509 -req -in pki-sha384-leaf.csr -CA pki-int.pem -CAkey pki-int.key \
  -set_serial 53 -not_before $L -not_after $LE -sha384 \
  -extfile pki-p384-ext.cnf -extensions leaf -out pki-sha384-leaf.pem 2>/dev/null
openssl x509 -req -in pki-sha384-leaf.csr -CA pki-int.pem -CAkey pki-int.key \
  -set_serial 54 -not_before $L -not_after $LE -sha384 \
  -sigopt rsa_padding_mode:pss -sigopt rsa_pss_saltlen:48 -sigopt rsa_mgf1_md:sha384 \
  -extfile pki-p384-ext.cnf -extensions leaf -out pki-pss384-leaf.pem 2>/dev/null

# ---- and the pairing this build refuses: ecdsa-with-SHA384 signed by a P-256 key. It is legal
# X.509 and openssl makes it without complaint; verifying it would mean FIPS 186-4 6.4's digest
# truncation, which ec.nt does not do, so verify.nt refuses it with both the algorithm and the
# curve named. The mirror image — ecdsa-with-SHA256 by a P-384 key — is pki-ec384-leaf.pem, which
# pki-ec-gen.sh already makes.
openssl req -new -key pki-ec-leaf.key -subj "$LS" -sha384 -out pki-p256x-leaf.csr 2>/dev/null
openssl x509 -req -in pki-p256x-leaf.csr -CA pki-ec-root.pem -CAkey pki-ec-root.key \
  -set_serial 55 -not_before $L -not_after $LE -sha384 \
  -extfile pki-p384-ext.cnf -extensions leaf -out pki-p256x-leaf.pem 2>/dev/null

rm -f pki-p384-leaf.csr pki-sha384-leaf.csr pki-p256x-leaf.csr pki-p384-ext.cnf

hdr() {
  f=$1; shift
  { echo "# $*"
    echo "# Regenerate the SHA-384 fixtures with:  sh tests/data/pki-p384-gen.sh  (OpenSSL 3.6.2),"
    echo "# which carries the exact command for this file. Dates are pinned there on purpose."
    cat "$f"; } > "$f.h" && mv "$f.h" "$f"
}
hdr pki-p384-root.pem "a P-384 root, self-signed with ecdsa-with-SHA384, CA:TRUE pathlen:0"
hdr pki-p384-leaf.pem "a P-384 leaf issued by pki-p384-root with ecdsa-with-SHA384; SAN DNS:leaf.neant.test, IP:127.0.0.1"
hdr pki-sha384-leaf.pem "pki-leaf's key and subject, issued by pki-int with sha384WithRSAEncryption"
hdr pki-pss384-leaf.pem "the same, issued with RSASSA-PSS over SHA-384, MGF1-SHA384, salt 48"
hdr pki-p256x-leaf.pem "ecdsa-with-SHA384 signed by a P-256 key: a pairing this build refuses BY NAME"
for k in pki-p384-root.key pki-p384-leaf.key; do
  { echo "# A THROWAWAY fixture key, committed on purpose: \`openssl s_server\` needs"
    echo "# pki-p384-leaf.key to complete a real ecdsa_secp384r1_sha384 handshake in src/main.rs's"
    echo "# end-to-end test, and a key that only ever signs certificates for names under"
    echo "# .neant.test buys hermetic tests for nothing. Regenerate:  sh tests/data/pki-p384-gen.sh"
    cat "$k"; } > "$k.h" && mv "$k.h" "$k"
done
chmod 644 pki-p384-*.pem pki-p384-*.key pki-sha384-leaf.pem pki-pss384-leaf.pem pki-p256x-leaf.pem
