#!/bin/sh
# The CRL fixtures: what src/neant/crypto/crl.nt parses and verifies. Run from the crate root:
#   sh tests/data/pki-crl-gen.sh    (OpenSSL 3.6.2)
#
# A SEPARATE script, as pki-ec-gen.sh and pki-ed-gen.sh are, so that regenerating a CRL never
# regenerates a key some other fixture's vector is pinned to. It reuses the RSA CA that
# pki-ed-gen.sh committed, and revokes a certificate that script also committed, so the two files
# it writes are the only things that change.
#
# `openssl ca -gencrl` needs a CA database rather than just a key, so one is built here and thrown
# away: index.txt lists the certificates the CA has issued and their state, V for valid and R for
# revoked with the date it happened.
set -e
cd "$(dirname "$0")"
umask 077
D=$(mktemp -d); trap 'rm -rf "$D"' EXIT

cat > "$D/ca.cnf" <<'EOF'
[ca]
default_ca = fixture
[fixture]
database        = $ENV::D/index.txt
serial          = $ENV::D/serial
crlnumber       = $ENV::D/crlnumber
default_md      = sha256
default_crl_days= 30
policy          = anything
[anything]
EOF
: > "$D/index.txt"; echo 01 > "$D/serial"; echo 01 > "$D/crlnumber"
export D

# serial 54 is pki-sha512-leaf, the leaf pki-ed-gen.sh issued from this CA. Mark it revoked.
# The columns are: state, notAfter, revocation date, serial, filename, subject.
printf 'R\t350101000000Z\t260301000000Z\t36\tunknown\t/C=US/O=neant fixtures/CN=leaf.neant.test\n' > "$D/index.txt"
openssl ca -config "$D/ca.cnf" -gencrl -cert pki-sha512-root.pem -keyfile pki-sha512-root.key \
  -out pki-crl-revoked.pem -crldays 3650 2>/dev/null

# the same CA with nothing revoked: an empty list has no revokedCertificates field at all, which is
# the OPTIONAL this parser most easily gets wrong
: > "$D/index.txt"
openssl ca -config "$D/ca.cnf" -gencrl -cert pki-sha512-root.pem -keyfile pki-sha512-root.key \
  -out pki-crl-empty.pem -crldays 3650 2>/dev/null

# and one that expired long ago, to exercise the staleness check
: > "$D/index.txt"
faketime() { :; }
openssl ca -config "$D/ca.cnf" -gencrl -cert pki-sha512-root.pem -keyfile pki-sha512-root.key \
  -out pki-crl-stale.pem -crldays 1 -crlhours 0 2>/dev/null || \
openssl ca -config "$D/ca.cnf" -gencrl -cert pki-sha512-root.pem -keyfile pki-sha512-root.key \
  -out pki-crl-stale.pem -crldays 1 2>/dev/null

hdr() {
  f=$1; shift
  { echo "# $*"
    echo "# Regenerate with:  sh tests/data/pki-crl-gen.sh   (OpenSSL 3.6.2)."
    cat "$f"; } > "$f.h" && mv "$f.h" "$f"
}
hdr pki-crl-revoked.pem "a CRL from pki-sha512-root listing serial 0x36 (pki-sha512-leaf) as revoked"
hdr pki-crl-empty.pem "the same CA's CRL with nothing on it: no revokedCertificates field at all"
hdr pki-crl-stale.pem "a CRL whose nextUpdate is one day out, for the staleness check"
chmod 644 pki-crl-*.pem
