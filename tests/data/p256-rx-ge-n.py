#!/usr/bin/env python3
# The one ECDSA P-256 case that cannot be found by sampling: a valid signature whose recovered
# R.x is >= n, so that the verifier's final test really has to be (R.x mod n) = r and not a bare
# comparison. Run from the crate root:  python3 tests/data/p256-rx-ge-n.py
#
# Why it has to be constructed. R.x is a value mod p and r is a value mod n, and for P-256
# p - n = 0x4319055358e8617b0c46353d039cdaae -- 127 bits against a 256-bit field, so a randomly
# generated signature lands in [n, p) about once in 2^129. Every vector anyone will ever sign with
# openssl has R.x < n already, which is exactly why a verifier that forgets the reduction passes
# every hand-written test and is still wrong.
#
# The construction does not need a discrete log. Pick any x in [n, p) that is on the curve, call
# that point R; pick u1 and u2; then the public key that makes u1*G + u2*Q land on R is
# Q = u2^-1 (R - u1*G), and the signature that produces those u1 and u2 is s = r/u2, e = u1*s.
# e is handed to the verifier as the digest, which is what makes it a 32-byte value and not a hash
# of anything -- ecdsaVerifyP256 takes an already-computed digest, so nothing here has to invert
# SHA-256. A corollary worth knowing: r = R.x - n is always below p - n, so r in such a signature
# always has sixteen leading zero bytes.
p = 0xffffffff00000001000000000000000000000000ffffffffffffffffffffffff
n = 0xffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551
b = 0x5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b
G = (0x6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296,
     0x4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5)

def add(P, Q):                                   # textbook affine, for a reference nobody times
    if P is None: return Q
    if Q is None: return P
    if P[0] == Q[0] and (P[1] + Q[1]) % p == 0: return None
    l = (3*P[0]*P[0] - 3) * pow(2*P[1], p-2, p) % p if P == Q \
        else (Q[1] - P[1]) * pow(Q[0] - P[0], p-2, p) % p
    x = (l*l - P[0] - Q[0]) % p
    return (x, (l*(P[0] - x) - P[1]) % p)

def mul(k, P):
    R, k = None, k % n
    for bit in bin(k)[2:]:
        R = add(R, R)
        if bit == '1': R = add(R, P)
    return R

def neg(P): return (P[0], (-P[1]) % p)

assert p < 2*n                                   # so an x in [n, p) reduces to exactly x - n
x0 = n + 0x2b3e964a5562b92cbe850cd3e43f7cf3      # anywhere in the 127-bit gap; arbitrary
while True:                                      # walk up to the first x that is on the curve
    rhs = (x0*x0*x0 - 3*x0 + b) % p
    y0 = pow(rhs, (p+1)//4, p)                   # p = 3 mod 4, so this is the square root
    if y0*y0 % p == rhs: break
    x0 += 1
assert n <= x0 < p
R, r = (x0, y0), x0 % n
assert r == x0 - n and r != x0

u1, u2 = 0x2b3e964a5562b92cbe850cd3e43f7cf3, 0x55e6dea154b4700c66b26fbb90b91818
Q = mul(pow(u2, n-2, n), add(R, neg(mul(u1, G))))
s = r * pow(u2, n-2, n) % n
e = u1 * s % n
assert 0 < r < n and 0 < s < n

w = pow(s, n-2, n)                               # and check it the way a verifier would
Rv = add(mul(e*w % n, G), mul(r*w % n, Q))
assert Rv == R and Rv[0] % n == r and Rv[0] != r

print("pub    04%064x%064x" % Q)
print("digest %064x" % e)
print("r      %064x" % r)
print("s      %064x" % s)
print("R.x    %064x   what a verifier missing the reduction compares against" % x0)
