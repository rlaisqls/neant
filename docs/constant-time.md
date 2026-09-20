# Constant time, and a checker for it

The property this language is built around. Summary in [../README.md](../README.md).


Nothing in this tree is constant-time. That sentence appears beside the crypto because it is true,
and `jitct` is what turns it from a disclaimer into a measurement:

```
jitct {[a;b] a bxor b}
"jitct: the code has 1 conditional branches for 0 counted loops (at words 15) — a deopt, a bounds
 check, or a vector index whose bound has to be tested"
```

`jitct f` compiles `f` in exactly the environment the JIT compiles it in — the same trampoline
addresses, the same inlinable-primitive slots — and answers `""` only when the *emitted machine
code* has no data-dependent control flow. It is a check on the bytes, not an argument about the
source, so it cannot be talked around. Two rules:

- the bytecode may hold no `Jmpf`. Every `if`, `$[..]` and `while` compiles to one and each is a
  branch on a value the function was handed. `do[n;..]` is allowed when `n` is a literal: then the
  branch is on a counter that runs the same way for every input.
- the emitted code may hold no conditional branch beyond one back-edge per such loop — no `B.cond`,
  `CBZ`/`CBNZ`, `TBZ`/`TBNZ`. Decoding the words rather than enumerating the cases means a deopt, a
  bounds check, or anything a later change to `arm64.nt` starts emitting is caught without being
  anticipated.

**What makes a function pass** is a discipline the compiler can see, not a mode a caller has to
remember. `a bxor b` used to compile to the `eor` and then a comparison of its *result* against the
int-null sentinel, branching to a deopt — necessary, because a value equal to the sentinel would be
read as a null by a later `+`, `-` or `*`, which the interpreter propagates and compiled code does
not. But a function that never uses those three cannot reach that disagreement, so `jitHasArith`
(src/neant/jit/arm64.nt) looks for them once and the checks after the bit operations are simply not
emitted. Nothing is marked; the property is the code's own. A shift by a literal drops its range
guard the same way, which is what a rotation in a hash is made of.

So the answer is now "yes" for the shapes constant-time code is actually written in:

```
jitct {[a;b] (a band b) bor a bxor b}    // ""
jitct {[x] x shr 63}                     // ""
jitct {[a] n: 0; do[8; n: n bxor a]; n}  // ""
jitct {[a;b] (a bxor b) + 1}             // the check is back, and named
```

`src/neant/stdlib/ct.nt` is the set of primitives built on it — `ctMask`, `ctSel`, `ctEqBit`,
`ctLtBit` (unsigned, which is what a limb is), and the `ctAcc` fold a tag comparison needs, since
`~` stops at the first difference. Each is asserted constant-time in `tests/ct.nt` alongside its
answer, so a helper rewritten with an `if`, or a `+` slipped in where `badd` belongs, fails the
suite rather than quietly costing the property. They are written out rather than composed: a
compiled function calling another goes through a trampoline that checks whether the callee bailed
out, and that check is a branch — one that could never be taken between two functions that cannot
deopt, which is a refinement the checker does not make yet.

This proves one property and not constant-time in general: `MUL` is constant-time on the cores this
targets but not architecturally required to be, and nothing here says anything about what a caller
does before or after. What it does mean is that the crypto can be *moved* onto ground where the
claim is checked rather than argued.

### What has been moved onto it

Two places branched on a secret, and both are now written without the branch.

**Authenticator comparison.** `aeadDecrypt` checked its Poly1305 tag with `~`, which is Rust's
`Vec<u8>` equality: it memcmps and returns the moment it finds a difference, so the time it takes
counts how many leading bytes of a forged tag were right. That turns forging a 16-byte tag from one
guess in 2^128 into sixteen searches of 256. `macEq` (src/neant/crypto/crypto.nt) xors the two
vectors and sums the result — whole-vector primitives with no early exit — and the two TLS
`Finished` checks, client and server, go through it too.

**The ECDSA nonce.** `ecShamir` does an addition per *set* bit of the scalar. For verification that
is free speed and every input is public. For signing the scalar is the nonce `k`, and a handful of
`k`'s bits recovers the private key, because the lattice attacks on biased ECDSA nonces need far
less than a handful. `ecCtMul` (src/neant/crypto/ec.nt) is double-and-add-always instead: every bit
costs one doubling and one addition, the addition happens whatever the bit is, and `ctSel` decides
with a mask which result survives. The addition formula's early returns are gone — `ecCtDbl` needs
none, since `(X:Y:0)` doubles to `z3 = 0` on its own — and the one reachable degenerate case, the
accumulator still being the point at infinity, is put back as a mask.

The difference, measured on P-256 with two 256-bit scalars of the same width, one with a single set
bit and one with 255 (`tests/ct.nt`):

```
              1 bit set    255 bits set
ecShamir         218 ms         455 ms     <- the side channel, in one number
ecCtMul          592 ms         589 ms
```

Signing costs about 1.8x what it did. That is the price, and it is paid once per signature.

**The x25519 swap.** A Montgomery ladder does the same field operations in the same order for every
scalar, and this one always did — but it swapped its two accumulators with an `if`, so the one thing
left to see was whether two consecutive bits of the private key differ, which is the key up to one
bit. `fcswap` computes both arms and selects with a mask. Being exact about what this is worth:
measured the same way, a scalar that made the old code swap at every step ran 630 ms against 604 ms
for one that almost never did. Four percent is not something that clock can separate from noise,
which is why `tests/ct.nt` asserts nothing about it. The branch was removed because a branch on key
bits is a branch on key bits and a branch predictor is a much finer instrument than a millisecond
counter — not because the wall clock objected. It costs about 15%.

**The RSA private exponent.** `bnModExpL` multiplies for a set bit and does nothing for a clear one.
Where the exponent is public that is the right trade — 65537 for a verification, n-2 for a modular
inverse — and where it is `dP` or `dQ` it is the private key, read out by a clock.
`bnModExpCtL` (src/neant/crypto/bignum.nt) multiplies at every bit and keeps the result with a mask,
over a bit count the caller passes rather than `bnBits` of the exponent, because stripping the
leading zeros would announce where the top set bit is. `rsaPriv` also blinds the exponent: `dP +
r(p-1)` is congruent mod `p-1`, so the answer is unchanged — signatures stay byte-identical, which
is how the pinned OpenSSL-verified vectors in `tests/sign.nt` still pass — while the bit pattern
walked differs at every call.

```
                 1 bit set    255 bits set
bnModExpL           171 ms         306 ms
bnModExpCtL         406 ms         405 ms
```

RSA-2048 signing goes from 18.3 ms to 31.2 ms a signature, about 1.7x.

**The conditional subtract.** `bnMontFinL` — the last step of every Montgomery multiply, so the
innermost thing in every exponentiation and every point addition — used to compare its result
against the modulus limb by limb, stopping at the first pair that differed, and subtract only if it
had to. That is Kocher's channel and Brumley and Boneh's: the one a chosen message steers. It now
always subtracts, into scratch, and the borrow selects which result is kept. About 9%, and it lifts
RSA-2048 signing to 31.2 ms. It is not `jitct`-checkable and cannot be — limb arithmetic is `+` and
`-`, so the compiled code carries the int-null checks those bring, branching on a sentinel that a
limb value never takes. Perfectly predicted is an argument, not a proof, and the checker deals only
in emitted bytes.

**What is still open**: the base is not blinded. `bnMontMul`'s
reduction ends in a conditional subtract whose frequency depends on its operands, which is what an
attacker choosing messages and timing the answers exploits. The fix is to sign `m * r^e` and divide
the result by `r`, and that needs `r^-1 mod n`: a binary extended GCD wants a signed representation
that `bignum.nt` does not have, and Fermat per prime would cost two more full exponentiations.
Not written. So what is closed is the first-order channel — the secret steering control flow — in
all four places it existed, and what is open is a second-order one on intermediate values.

**What is still not constant-time** in the field layer: `bnMontMul` trims its own result, so a value
with a zero top limb makes the next multiply cheaper, and `bnAddModL`'s conditional subtract is a
branch. The `efCt*` wrappers pad every operand back to the curve's full limb width, which closes the
first of those inside the loop; the second remains, as a second-order channel on intermediate
coordinates rather than a first-order one on the scalar's own bits.

