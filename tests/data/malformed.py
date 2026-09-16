# Regenerate tests/data/malformed.pem:  python3 tests/data/malformed.py > tests/data/malformed.pem
# Each block is tests/data/selfsigned-rsa.pem's DER, damaged one way, re-wrapped as PEM.
import base64, re, sys, textwrap

pem = open("tests/data/selfsigned-rsa.pem").read()
der = base64.b64decode(re.search(r"-----BEGIN CERTIFICATE-----(.*?)-----END CERTIFICATE-----", pem, re.S).group(1))

# the two AlgorithmIdentifiers are identical, so the last occurrence is the outer one
SHA256RSA = bytes.fromhex("06092a864886f70d01010b")
def swap_outer_alg(b):
    i = b.rindex(SHA256RSA)
    return b[: i + 10] + b"\x0c" + b[i + 11 :]

def wrap(b, why):
    body = "\n".join(textwrap.wrap(base64.b64encode(b).decode(), 64))
    return f"{why}\n-----BEGIN CERTIFICATE-----\n{body}\n-----END CERTIFICATE-----\n"

# der[0] is 0x30, der[1] is 0x82, der[2:4] the two length octets.
n = int.from_bytes(der[2:4], "big")
blocks = [
    # 0: the top-level length claims four more octets than the file holds.
    (der[:2] + (n + 4).to_bytes(2, "big") + der[4:], "0 length runs past the end of the input"),
    # 1: the same length in three octets instead of two -- long form, but not the shortest one.
    (der[:1] + b"\x83" + (n).to_bytes(3, "big") + der[4:], "1 non-minimal long-form length"),
    # 2: 0x80, the indefinite length: legal BER, forbidden in DER.
    (der[:1] + b"\x80" + der[4:] + b"\x00\x00", "2 indefinite length (BER, not DER)"),
    # 3: one octet of padding after a complete, otherwise valid certificate.
    (der + b"\x00", "3 trailing octet after the top-level element"),
    # 4: the outer signatureAlgorithm restated as sha384WithRSAEncryption while tbsCertificate still
    #    says sha256 -- same encoded length, so only RFC 5280 4.1.1.2's equality check catches it.
    (swap_outer_alg(der),
     "4 signatureAlgorithm disagrees with the one inside tbsCertificate"),
]
sys.stdout.write("Deliberately malformed certificates, one per block, in the order the tests assert.\n"
                 "Regenerate with:  python3 tests/data/malformed.py > tests/data/malformed.pem\n\n")
for b, why in blocks:
    sys.stdout.write(wrap(b, why) + "\n")
