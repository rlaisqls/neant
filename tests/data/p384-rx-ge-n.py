#!/usr/bin/env python3
# The one ECDSA P-384 case that cannot be found by sampling: a valid signature whose recovered
# R.x is >= n, so that the verifier's final test really has to be (R.x mod n) = r and not a bare
# comparison. Run from the crate root:  python3 tests/data/p384-rx-ge-n.py
#
# The P-384 twin of tests/data/p256-rx-ge-n.py, and the same bug is possible again because the same
# code runs both curves: ec.nt's ecVerify does the reduction once, for whichever curve it is handed.
# If anything, P-384 hides the bug better. R.x is a value mod p and r is a value mod n, and here
# p - n = 0x389cb27e0bc8d21fa7e5f24cb74f58851313e696333ad68c -- 190 bits against a 384-bit field,
# so a randomly generated signature lands in [n, p) about once in 2^194, against P-256's 2^129.
# Every vector anyone will ever sign with openssl has R.x < n already.
#
# The construction does not need a discrete log. Pick any x in [n, p) that is on the curve, call
# that point R; pick u1 and u2; then the public key that makes u1*G + u2*Q land on R is
# Q = u2^-1 (R - u1*G), and the signature that produces those u1 and u2 is s = r/u2, e = u1*s.
# e is handed to the verifier as the digest, which is what makes it a 48-byte value and not a hash
# of anything -- ecdsaVerifyP384 takes an already-computed digest, so nothing here has to invert
# SHA-384. A corollary worth knowing: r = R.x - n is always below p - n, so r in such a signature
# always has twenty-four leading zero bytes.
p = 0xfffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffeffffffff0000000000000000ffffffff
n = 0xffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf581a0db248b0a77aecec196accc52973
b = 0xb3312fa7e23ee7e4988e056be3f82d19181d9c6efe8141120314088f5013875ac656398d8a2ed19d2a85c8edd3ec2aef
G = (0xaa87ca22be8b05378eb1c71ef320ad746e1d3b628ba79b9859f741e082542a385502f25dbf55296c3a545e3872760ab7,
     0x3617de4a96262c6f5d9e98bf9292dc29f8f41dbd289a147ce9da3113b5f0b8c00a60b1ce1d7e819d7a431d7c90ea0e5f)

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
x0 = n + 0x1d3f0a6c2e58b91477c0fe3ab526d84c9f01a7e33b6cd215   # anywhere in the 190-bit gap
while True:                                      # walk up to the first x that is on the curve
    rhs = (x0*x0*x0 - 3*x0 + b) % p
    y0 = pow(rhs, (p+1)//4, p)                   # p = 3 mod 4, so this is the square root
    if y0*y0 % p == rhs: break
    x0 += 1
assert n <= x0 < p
R, r = (x0, y0), x0 % n
assert r == x0 - n and r != x0

u1 = 0x2f1c8a04b6e93d5718ac60f2b84d17e3905ca6b2d3f04817
u2 = 0x63b90e2d14fa78c50d6e2b91c7043f8a25de16b0947cf3a1
Q = mul(pow(u2, n-2, n), add(R, neg(mul(u1, G))))
s = r * pow(u2, n-2, n) % n
e = u1 * s % n
assert 0 < r < n and 0 < s < n

w = pow(s, n-2, n)                               # and check it the way a verifier would
Rv = add(mul(e*w % n, G), mul(r*w % n, Q))
assert Rv == R and Rv[0] % n == r and Rv[0] != r

print("pub    04%096x%096x" % Q)
print("digest %096x" % e)
print("r      %096x" % r)
print("s      %096x" % s)
print("R.x    %096x   what a verifier missing the reduction compares against" % x0)
