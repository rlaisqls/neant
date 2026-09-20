# Performance log

A record of what was measured and what it bought — kept because the numbers are the argument, not because a reader needs it to start.


- `x,: y` compiles to Take+join so appends are in place — 20k appends, 444ms → 1ms.
- Globals are interned to slots at load, so `LoadG` is an index, not a hash. Execution stacks are pooled.
- `?` `distinct` `group` hash atoms — 200k ints over 1000 keys: distinct 159ms → 6ms, group 198ms → 10ms.
  Nested keys fall back to a scan.
- `ss` narrows to the positions whose first character matches and checks only those, instead of
  taking a slice and matching at every position; a one-character pattern is a single vector compare
  and no slicing at all. Splitting a 215KB buffer on `"\n"`: 103ms → 10ms, and `ss` alone 95ms → 5ms.
  This is the shape a PEM file, a header block or a CSV is read with, so it is `vs`, `ssr`, `like`
  and everything built on them.
- Atom lookup in a typed vector scans the raw elements instead of boxing the vector. (`x in y` is a
  `?` over `Syms`, and the boot compiler's ``k in `const`verb...`` dispatch chains run it per AST node.)
- Two int atoms through `+ - * & | < > =` skip the shape/broadcast machinery, and through
  `band bor bxor shl shr` as well — those are named builtins, so they carry the fast function on
  `PrimDef` rather than being matched by name.
- **A call to anything that is not a user function — a primitive, an adverb, `x[i]` on a vector or a
  dict — reads its arguments straight off the stack.** The general path builds an argument vector that
  becomes the callee's locals and returns to the pool, but `call` consumed and dropped that vector for
  a primitive, so every `a bxor b` and every `w[i]` allocated. This was the single largest win.
- `x[i]` and `x[i]: v` for a typed vector indexed by an int vector gather and scatter in place.
  The general path boxes every index and element into a `Value`, indexes one at a time, then re-detects
  the type in `pack` — the shape `acc[i+til 12] +: a[i]*b` that the field arithmetic is built out of.

Together: a `while` iteration
costs 26ns. On the crypto in `src/neant/crypto/crypto.nt`, per 64KB: SHA-256 284ms → 119ms, ChaCha20 115 → 68,
Poly1305 61 → 34, the AEAD 198 → 103; X25519 76ms → 42, and a TLS 1.3 handshake against OpenSSL
169ms → 97ms. Allocation went from ~34% of samples to under 1%; what is left is the dispatch loop
itself and `Value` clone/drop.

Those absolute figures are **not reproducible on the machine this is developed on** and should be
read as ratios only. `crypto.nt` has not changed since (only the move into `src/neant/crypto/`), and
checking out that same commit here measures SHA-256 at 195ms per 64KB rather than 119ms — so the
numbers above came from different hardware. Measured here on that same code: SHA-256 **233ms** per 64KB,
ChaCha20-Poly1305 **21.6ms** per 10KB, X25519 **47.5ms** — all three are now 1.0ms, 0.38ms and
3.05ms, on the compiled kernels below. The 196ms → 221ms *within* the interpreted implementation is
real, and it is one commit: `fbdeb99`, which converted `Value` from `Rc` to `Arc` so that `spawn`
could exist; the interpreted figure was flat from there until the kernels replaced it. See
"Concurrency" for the trade — it is the price of threads, not a regression anyone can take back.

Runtime errors carry a line table, which costs ~10% of compile throughput. A frame is named by its
caller's `LoadG`, so the bytecode carries positions but no names.

#### The parser really is one pass now

"No backtracking" at the top of this file was not true. `expr` saw `name [` and parsed the bracket
speculatively, to find out whether it was looking at `x[i]: v` or at `x[i]`, and threw the result
away when it was not. `while` and `if` are *names*, so every bracketed block went through it — and a
block inside a block inside a block was parsed eight times. Measured on 1.6KB of statements wrapped
in `d` nested `while`s, where only the wrapper changes: 12ms at depth 0 and **228ms at depth 6**,
doubling with every level.

It is a token scan now (`pmatch`): find the `]` that closes the `[`, look at what follows, and only
look inside the bracket when a `:` says it is an assignment. `resolve` was quadratic for a separate
reason — it recursed once per term and copied the remaining items each time, so a long expression
was O(n²) to parse and ran out of stack somewhere past a thousand terms — and now walks the items
backwards, keeping the last two results, because the rules consume one item or two.

What the two are worth, measured: the same depth-6 block is 9ms rather than 228ms; loading the ten
TLS modules is **373ms rather than 539ms**, with `tls.nt` — the file with the deepest nesting — going
106ms to 39ms; and the self-hosted rebuild of all the sources is **611ms rather than 767ms**.

#### The bignum arithmetic runs as compiled scalar loops

`src/neant/crypto/bignum.nt` was written as vector column operations — `acc[i+til nb] +: a[i]*b`,
one per limb of `a`, which is the fastest shape an *interpreter* has. Every one of them allocates
intermediates and makes several passes over memory, and none of them is a loop the JIT can take.
With `x[i]` inlined, a written vector uniquely owned at entry, a vector as a return value and the
bit builtins as instructions, the same arithmetic is expressible as plain scalar loops over limbs
that the whole-function tier compiles: `bnMulL` is the schoolbook double loop, `bnRedcL` the
Montgomery reduction, `bnMsubL`/`bnAddBackL` Knuth D's multiply-subtract and add-back,
`bnCarryL` one sequential carry pass instead of repeated vector rounds, and `bnCmpL`, `bnAddL`,
`bnSubL`, `bnAddModL`, `bnSubModL`, `bnMontFinL`, `bnTrimL`, `bnPackL`, `bnUnpackL` the operations
around them that a ten-limb field spends most of its time in. The vector forms stay in the file as
`bnMulV`, `bnMontMulV` and `bnCarryV`, and `tests/bignum.nt` checks thousands of random operands
against them — plus `u = q*v + r` for the division and a plain multiply-and-reduce for Montgomery,
which are stronger statements than agreeing with another of my own loops. Every crypto vector in
`tests/crypto.nt`, `tests/p256.nt`, `tests/p384.nt` and `tests/verify.nt` is exact and unchanged.

The tier compiles a function after 64 calls, and one RSA-2048 verification makes about twenty, so
`bnWarm[]` at the bottom of the file calls each kernel eighty times with two-limb operands. That is
~19ms of one-time compilation at load (the file's own parse is ~80ms, and was ~27ms before it grew),
and without it the first verification would run the whole schoolbook product on the interpreter,
which is far slower than the vector form it replaced.

Measured on this machine against b093293, every number the minimum of five runs:

| | before | after |
|---|---|---|
| `bnMul`, 2048×2048 bits | 13.5–15.1 ns per limb-multiply | **1.60 ns** |
| `bnMontMul`, 2048 bits | 15.4–16.8 ns per limb-multiply | **1.12 ns** |
| RSA-2048 verify (e=65537) | 6.9–7.8 ms | **0.50 ms** |
| ECDSA P-256 verify | 181 ms | **21.5 ms** |
| `x[i]*y[i]` in a compiled loop | 2.70 ns/iteration | **0.90–1.00 ns** |
| a compiled scalar loop | 0.90 ns/iteration | 0.90–0.95 ns |
| SHA-256 over 64KB, trust-store parse | 236 ms, 218 ms | 224 ms, 219 ms (untouched) |

Against OpenSSL on the same core, RSA-2048 goes from 580x to **39x** (12.7µs) and ECDSA P-256 from
4600x to **550x** (38.9µs). The compiled scalar loop is unchanged because it is bound by its four
branches rather than its instruction count — see the null-sentinel measurement above.

What is left is not one thing. Of 0.50ms for an RSA verification, ~0.36ms is the twenty modular
multiplications and ~0.30ms of that is the two compiled kernels, so the verification is now ~70%
inner loop and the honest gap divides in two. **Six of the remaining 39x is the limb width**: a
product must stay exact in an i64, which caps a limb at 26 bits against OpenSSL's 64, and (2048/26)²
is six times (2048/64)². That is a property of the *language* — an i64 is what a number is here —
not of the compiler, and closing it needs either a 128-bit product or Karatsuba, which is fewer
products rather than cheaper ones. The other ~7x is that a compiled limb product takes about four
cycles against the one hand-written assembly gets: three inlined loads with their bounds checks, a
multiply, an add, two counter increments and their null checks, and nine branches an iteration. For
P-256 the split is different again — ten limbs is 200 products per modular multiplication, ~0.26µs
of arithmetic inside a 1.9µs operation, so it is dominated by what surrounds each call (a fresh
`Ints` per result, the entry marshalling, `bnTrim`) rather than by the arithmetic.

**A wider limb was measured and does not pay.** 32 bits is not available at all — a 32×32 product is
2^64, which is not exact in an i64 — and 26 is already the widest the *column* form allows, since a
column is min(na;nb)·2^(2b) and at 27 bits an RSA-4096 reduction would reach 2^62.25 and overflow.
What a wider limb would buy has to be bought with a carry settled on every product instead, and that
was timed as the same loop with `band`/`shr` in it: 1.84ns per limb product against 1.12ns. At 2048
bits that is 4489 products at 1.84ns = 8.3µs against 6241 at 1.12ns = 7.0µs — 19% *worse*, because
64% more work per product does not pay for 28% fewer of them. Fewer products has to come from
Karatsuba, not from the radix.

#### The hash and the stream cipher run as compiled scalar loops

`src/neant/crypto/crypto.nt` got the same treatment `bignum.nt` did, for the same reason and with
the same recipe. `shaBlock` was written as a vector schedule grown by append, `chachaBlock` as four
column vectors through `qround`, `p5mul` and `fmul` as `acc[i+til n] +: a[i]*b` — the fastest shape
an interpreter has, and the three things a JIT cannot take. Every one of them was disqualified at
least twice over, and none of the disqualifiers needed a compiler change to remove. Measured one
cause at a time, 2M iterations of a `while` body, after the tier had taken the function:

| the body | cost | |
|---|---|---|
| `n: n+i` | 3ms | the baseline |
| `n: (n+i) band 65535` | 2ms | compiles since the bit builtins became instructions |
| `n: n+t[i band 63]`, `t` a **parameter** | 1ms | compiles |
| `w[i]: i`, `w` a **parameter** | 2ms | compiles |
| `n: n+G`, `G` a global int | **240ms** | a global read is not in the subset |
| `n: n+W[i mod 3]`, `W` a global vector | **652ms** | nor is indexing one |
| `n: n+w[i band 3]`, `w` a **scratch local** vector | **400ms** | only a parameter or capture is classifiable |
| `n: rr[n;3]+i`, `rr` a pure int helper | **209ms** | compiles, but a trampoline call is ~45ns |

So `M32`, `shaK`, `M26` and `M22` became parameters (`msk`, `k`), the message schedule, the ChaCha
state and both product accumulators became parameters allocated once as module-level scratch, and
`rotr32`/`rotl32`/`add32`/`qround` were written out by hand inside the kernels rather than called.
(`bnot` is not one of the six inlined builtins, so `msk bxor x` stands in for it on a masked word.)
The kernels are `shaBlocksL` — the whole message, every block, in one call — `chachaXorL`, which
produces the keystream and xors it in the same pass, `polyBlocksL` for the full 16-byte blocks, and
`fcarryL`/`fmulL` for the 2^255-19 field. The column forms stay in the file as `shaBlockV`,
`sha256V`, `chachaBlockV`, `poly1305V`, `fcarryV` and `fmulV`, and `tests/crypto.nt` checks the two
against each other across every length either side of a block boundary and on random field
elements. Every published vector in `tests/lang.nt` is unchanged and exact.

**A boot file may not warm its own kernels at load, and failing quietly is the whole trap.** The
tier compiles after 64 calls; one `sha256` of any size is *one* call to `shaBlocksL`, so a TLS
handshake's thirty hashes would never reach the threshold and `bnWarm`'s trick is needed here too.
But `crypto.nt` is `BOOT_FILES[9]` and `src/neant/jit/arm64.nt` is `BOOT_FILES[10]`: a kernel driven
past 64 calls while the image is still loading finds no `jitCompile` global to call, `jit::compile`
returns `None`, and `FnCode`'s `OnceLock` settles that as "never compile this" for the life of the
process. Nothing fails — every test still passes, the hash is simply eight times slower than it
should be, and the only symptom is a number. So each family warms on first *use* instead
(`shaWarm`, `chachaWarm`, `polyWarm`, `fieldWarm`), which costs ~8ms once on the first hash of a
process and nothing at startup.

X25519 is the exception that proves the rule and was worth measuring rather than assuming: one key
exchange is ~2800 calls to `fmulL`, so it tiers up 2% into its own first run whether or not anything
warmed it — 17ms cold against 3.05ms warm. The warm-up there buys only that 14ms of first-run
interpretation, not the compilation itself.

Measured on this machine against `8f04ace`, each the minimum of five runs of twenty-plus operations:

| per operation | before | after | |
|---|---|---|---|
| `sha256`, 64KB | 233.2 ms | **1.00 ms** | 233x |
| `sha256`, 4KB | 14.90 ms | **0.090 ms** | 166x |
| `hmac`, 32 bytes | 0.895 ms | **0.015 ms** | 60x |
| `chacha20`, 64KB | 95.2 ms | **1.70 ms** | 56x |
| `poly1305`, 64KB | 43.8 ms | **0.30 ms** | 146x |
| `aeadEncrypt`, 64KB | 138.2 ms | **2.45 ms** | 56x |
| `aeadEncrypt`, 10KB | 21.6 ms | **0.38 ms** | 57x |
| `x25519` | 47.5 ms | **3.05 ms** | 15.6x |
| `sha512`, 64KB | 110.9 ms | **0.90 ms** | 123x |
| verified TLS 1.3 handshake, www.google.com | 394–462 ms | **223–265 ms** | 1.8x |

Against OpenSSL 3.6.2 on the same core (`openssl speed -elapsed`, 2.46 GB/s for both at 16KB and
44355 X25519/s), SHA-256 over 64KB goes from 8760x to **38x** (26.6µs), ChaCha20-Poly1305 from
5180x to **92x** (26.7µs) and X25519 from 2110x to **135x** (22.5µs).

What is left is no longer the arithmetic. Of the ~240ms handshake, ~140ms is two network round trips
and ~80ms is three ECDSA verifications; `crypto.nt` is now a few milliseconds of it. Inside `x25519`
the 3.05ms is ~2550 `fmulL` calls at 0.7µs each and ~2000 `fadd`/`fsub`/`fmuls` at 0.4–0.5µs, where
the add still builds a twelve-element vector interpreted before handing it to the compiled carry;
folding a whole ladder step into one kernel would remove that, and cannot be done by calling the
field kernels from inside another compiled function — a callee that returns a vector deopts the
caller, so it would mean writing the nine multiplications out by hand.

