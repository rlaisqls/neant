# Crypto

What is implemented on the bytes, and against which vectors. The checked-property story is in [constant-time.md](constant-time.md).

## Bytes and crypto

```
0x0aff                       // byte literal
`byte$"hé"                   // 0x68c3a9   UTF-8 encode
`char$0x68c3a9               // "hé"       decode
`int$0x0aff                  // 10 255     arithmetic on bytes gives ints
key bxor data                // the bit verbs on two byte operands give bytes
```

`src/neant/crypto/crypto.nt` is pure neant on those: `sha256 hmac hkdfExtract hkdfExpand chacha20 poly1305
aeadEncrypt aeadDecrypt x25519`, all checked against the RFC vectors (measured on this machine:
SHA-256 **1.0ms** per 64KB, ChaCha20 **1.7ms** and Poly1305 **0.34ms** over the same 64KB, X25519
**3.0ms**). 32-bit words
live in ints masked after each sum; the 2^255-19 and 2^130-5 fields use 22- and 26-bit limbs so
products stay exact in an int. Every hot kernel is a scalar loop over indices that the
whole-function JIT tier compiles — see "The hash and the stream cipher run as compiled scalar
loops" under "Performance", and the file's own header for the three things that used to stop it.

`src/neant/crypto/sha512.nt` and `src/neant/crypto/ed25519.nt` (loadable, not in the boot image) add
**SHA-512, SHA-384 and Ed25519 verification** on top of that field — FIPS 180-4 and RFC 8032
vectors, ~68ms per signature:

```
load "src/neant/crypto/sha512.nt"; load "src/neant/crypto/ed25519.nt"
ed25519Verify[pub; msg; sig]       // 32-byte key, 64-byte signature -> 1b / 0b
hex sha512 `byte$"abc"
hex sha384 `byte$"abc"             // 48 bytes: the same compression function, another IV
```

64-bit words need no splitting: `band bor bxor shl shr` are exact on the raw `i64` pattern (`shr` is
logical, `shl` discards), and `badd` is the wrapping add — plain `+` would read `1 shl 63` as an int
null and poison the round. The curve reuses `fadd fsub fmul fsq finv fencode fdecode` unchanged, in
extended coordinates with the complete addition law, and Shamir's trick does both scalars in one pass
of 253 doublings. Scalars reduce mod L one bit at a time. Verification only: no signing, and nothing
is constant-time, which is what a verifier's all-public inputs allow.

SHA-384 is not a second hash. FIPS 180-4 5.3.4 and 6.5 define it as SHA-512's compression function
with a different initial hash value and the digest cut to 48 octets, so both hashes go through one
kernel.

**That kernel contains no `+`, `-` or `*`, and that is why it runs at all.** SHA-512 used to take
1800ns a byte against SHA-256's 20 — the same algorithm, ninety times slower, because it was written
a block at a time and grew its message schedule with `w,: ...`, which is not one of the two shapes a
vector slot may appear in and so refused the whole function. Rewriting it in `shaBlocksL`'s shape —
every buffer a parameter, the schedule written at `w[i]`, the whole message in one call — was not
enough on its own: it compiled and then **deopted on nearly every round**. A function that uses one
of those three operators makes the codegen check every bit operation's result against the int-null
sentinel, because a later `+` would read that pattern as a null; and the sentinel is
`0x8000000000000000`, which is exactly what `x shl 63` is whenever `x` is odd. A rotation does that
constantly. SHA-256 never met it because it masks every word to 32 bits, so none of its values can
be the sentinel; a 64-bit word has no room to be anything but the raw pattern.

So `badd` replaced every `+`, and the subtractions became indices that walk alongside `i` rather
than `i-15`. The same discipline `src/neant/stdlib/ct.nt` follows for constant-time code, arrived at
from the opposite direction.

```
sha512, 64KB     110.9 ms  ->  0.90 ms    123x
sha512, 1KB        2.12 ms  ->  0.060 ms   35x
```

At 6.9ns a byte it is now faster than SHA-256, which is what a 128-byte block of 64-bit words
should be. `sha512Block`, the block-at-a-time form, is kept and `tests/crypto.nt` checks the two
agree on 134 lengths, the way `shaBlockV` is kept beside `shaBlocksL`. It used to live inside
`ed25519.nt`, because nothing else needed it; certificates signed `ecdsa-with-SHA384` do, and
`verify.nt` has no business loading a curve it cannot verify in order to get a hash.

`src/neant/crypto/ec.nt` with `p256.nt` and `p384.nt` (loadable, not in the boot image) add
**ECDSA verification on P-256 and P-384**, which between them are what the public web signs with, on
`bignum.nt`'s modular arithmetic and `der.nt`'s reader:

```
{load "src/neant/crypto/",x} each ("bignum.nt";"der.nt";"ec.nt";"p256.nt";"p384.nt")
ecdsaVerifyP256[pub; digest; r; s]      // pub is the 65-byte SEC 1 point 0x04 || X || Y
ecdsaVerifyP256Der[pub; digest; sig]    // sig is the DER SEQUENCE { r, s } a certificate carries
ecdsaVerifyP384Der[pub; digest; sig]    // 97-byte point, 48-byte SHA-384 digest
```

Both curves are total: an r or s outside [1, n-1], a public key not on the curve or with a coordinate
at or above p, a compressed point (refused on purpose — RFC 5480 makes it optional and nothing ships
one), a digest of the wrong length, or DER that does not parse all come back `0b`, never a signal.
The final test is `(R.x mod n) = r` with the reduction actually done. Points are Jacobian so the
scalar multiplication inverts once at the end rather than once per step, both inversions are Fermat
through `bnModExp`, and Shamir's trick does the two scalars in one pass of 256 or 384 doublings.

**The second curve is a curve record, not a second implementation.** `ec.nt` holds the field, the
group law, the ladder, the point decoding and every range check; `p256.nt` and `p384.nt` are each
five hex constants and a handful of one-line wrappers. That was decided a change earlier, by
reusing `bignum.nt`'s Montgomery reduction instead of folding P-256's special prime the way
`crypto.nt` folds 2^255-19 — a measured decision, not an omission: every exponent in
p = 2^256-2^224+2^192+2^96-1 is a multiple of 32, so the Solinas fold is a limb permutation only at
a limb width dividing 32, and an exact i64 column caps that at 16 — sixteen multiply-accumulate
passes against `bignum.nt`'s ten. Measured per modular multiplication at the time: `bnMontMul` 25µs
(1.9µs now), the
16-bit Solinas multiply-and-fold 19µs *before* it brings a result in (-4p, 6p) back under 2^256,
which is about 5µs more. Level, so reuse won. A fixed-width fold would have had to be written again
over P-384's prime with its own carry analysis; a variable-length limb list did not have to be
written at all. P-384's own Solinas fold is ten 32-bit terms, so it would want 24 limbs of 16 bits
against the 15 of 26 bits used here — a worse ratio than P-256's 16-against-10, which came out
level, so there was nothing to trade and nothing was re-measured.

**One P-256 verification is ~21.5ms**, against ~0.50ms for RSA-2048 — the reverse of the compiled
ratio, because P-256 needs ~4900 modular multiplications where RSA-2048 with e=65537 needs twenty.
Both were an order of magnitude slower until the arithmetic stopped being interpreted vector
operations and became compiled scalar loops over limbs (["the bignum arithmetic runs as compiled
scalar loops"](#the-bignum-arithmetic-runs-as-compiled-scalar-loops)); P-384 is measured by the same
loop in the same VM rather than predicted, and its ratio to P-256 is 384 doublings against 256 and
15-limb multiplications against 10-limb ones.

An ECDSA algorithm is paired with exactly **one** curve here: `ecdsaSha256` means a P-256 key and
`ecdsaSha384` means a P-384 key. X.509 does not require that — a P-256 key may sign with SHA-384 —
but verifying a mismatched pair means FIPS 186-4 6.4's digest truncation, one more thing to get
subtly wrong in a verifier whose job is to say no, so a mismatch is refused with both the algorithm
and the curve named. In all seven public chains measured below, every `ecdsaSha384` signature is
checked against a P-384 key and every `ecdsaSha256` one against a P-256 key.

`src/neant/crypto/batch.nt` (loadable, not in the boot image) verifies **many P-256 signatures in
one call**, on `ec.nt`'s arithmetic — the single-signature path is untouched:

```
load "src/neant/crypto/batch.nt"
batchVerifyP256[keys; digests; sigs]        // 1b / 0b        sigs are (r;s) pairs
batchVerifyP256Bad[keys; digests; sigs]     // the indices that failed
batchVerifyP256DerWho[keys; digests; sigs]  // one boolean each, signatures as DER
batchVerifyP256Par[keys; digests; sigs; 20] // the same answers, over 20 OS threads
```

**The batch answer is per signature and it is exact** — it agrees with `ecdsaVerifyP256` signature by
signature, so a failing batch names which one rather than returning one bit. That is because this is
*not* the random-linear-combination batch of the literature, and it cannot be: that test needs the
point `R`, and an ECDSA signature carries only `r = x(R) mod n`. Lifting `r` back costs a square root
mod p and then hands you `(x, ±y)` — and the sign is not recoverable even in principle, because
`(r,s)` and `(r,n-s)` are both valid signatures of the same message under the same key and differ by
exactly that sign. A batch of N would have 2^N sign assignments. This is why Cheon–Yi and Karati–Das
state ECDSA batch verification for ECDSA\*, the variant that transmits `R`; secp256k1's recovery byte
is the same missing bit. The cost of not having it is real — a Pippenger bucket sum over the 2N+1
points would cut the ~4900 modular multiplications a verification takes here to roughly 900 — and the
gain is that there is no 2^-k soundness error to argue about and no bisection to find the bad one.

What the batch *does* exploit is that this is an array language. A field element for the whole batch
is laid out **lane-major**: `limbs` int vectors, one per 26-bit limb, each holding that limb of every
signature. The same schoolbook multiply then runs on vectors N times longer, and a ten-element vector
operation is almost all dispatch — measured here, `c + a*b` is 27.5 ns/element at length 10 and
1.41 ns/element at length 5120. `bnMontMul` costs 25.0µs one signature at a time and 0.651µs per
signature at 512 lanes, **38x**. Two things really are batch algorithms: one modular inversion for the
whole batch (Montgomery's trick as a binary product tree, replacing a Fermat `s^-1 mod n` per
signature), and a projective final comparison `X = r·Z²` — or `(r+n)·Z²`, the case `ec.nt`'s header
warns about — which removes the inversion mod p entirely.

Measured on this machine by `tests/bench.nt`, against the 512 real openssl signatures in
`tests/data/p256-batch.txt` (repeated to fill the 2048-lane row), with other work running on the
machine at the time — a re-run lands within about 15% either way:

| N | 1 thread, ms/sig | 1 thread, verify/s | 20 threads, ms/sig | 20 threads, verify/s |
|---:|---:|---:|---:|---:|
| 1 | 739.0 | 1.4 | 80.25 | 12.5 |
| 8 | 94.04 | 10.6 | 10.99 | 91.0 |
| 64 | 16.21 | 61.7 | 1.622 | 616.6 |
| 512 | 4.875 | 205.1 | 0.449 | 2225.1 |
| 2048 | 3.549 | 281.8 | 0.314 | 3180.6 |

against `ecdsaVerifyP256` one at a time at **180.9ms** (5.5 verify/s) and `openssl speed -seconds 3
ecdsap256` at **38.9µs** (25684 verify/s), both one core, both measured the same day. **So this does
not beat OpenSSL**: at its best — 2048 signatures in one call across 20 cores — it is 8x short of
OpenSSL's *single* core and about 160x short of OpenSSL on all twenty. What it does is close the gap
from 4650x to 8x, 577x more signatures per second out of the same machine, with no new Rust
primitive. It is also 4.1x *slower* than `ecdsaVerifyP256` for a single signature: the crossover is
at about four.

The twenty-thread column found something worth naming. `spawn` forks a snapshot of globals, and a
global holding a function holds the same `Arc<FnCode>` in every thread — and `FnCode` keeps the
tracing JIT's per-loop-header state behind a `Mutex` that the interpreter takes on **every backward
jump**. Twenty threads in the same hot loop queue on one lock: a batch of 64 across 20 threads runs
at 224 verify/s that way and at **611** when each thread re-`load`s the module first and gets its own
`FnCode`, which is what `batchVerifyP256Par` does. The fix that would make that unnecessary is in the
VM: `Compiled` and `Rejected` are write-once, so the hot path need not take the mutex at all.
Splitting *one* batch across threads is a different and much weaker idea: k threads means k times
fewer lanes to amortise the dispatch over, and it measures that way — 512 signatures take 2512ms in
one thread, 1818ms split four ways (1.38x) and 1967ms split twenty (1.29x), so four threads beat
twenty. `batchVerifyP256Par` is for a caller who has one batch and wants it sooner; the throughput
column above is twenty *independent* batches, which is the shape that scales.

`src/neant/crypto/der.nt` and `src/neant/crypto/x509.nt` (loadable, not in the boot image) read
**ASN.1 DER and X.509 certificates** — parsing only, no signature check and no chain building:

```
load "src/neant/crypto/der.nt"; load "src/neant/crypto/x509.nt"
c: x509Parse (pemLoad "tests/data/chain-google.pem")[0]
c`subject                          // "CN=www.google.com"          RFC 2253, as OpenSSL prints it
c`san                              // ("www.google.com")
hex sha256 c`tbs                   // the bytes the signature is over, as they arrived
(c`spki)`n                         // for `rsa: the modulus, big-endian, sign octet stripped
```

A parsed DER element carries the raw span it was cut from, because verifying a certificate hashes
the *original* encoding of tbsCertificate and a re-serialisation would not do. The reader is strict
on purpose — indefinite lengths, non-minimal lengths and tags, padded INTEGERs and OIDs, a BIT
STRING whose unused bits are set, a DEFAULT that DER should have omitted, and trailing bytes after
the top-level element all signal rather than being guessed at. `x509Parse` returns one dict, its
keys documented at the top of x509.nt; an unrecognised *critical* extension is reported in
`` `critUnknown `` rather than dropped, since silently ignoring one is how a verifier gets fooled.

**Two readers, held to each other.** `derTLV` builds a dict per element and `derKids` walks the
children one at a time; `derScan` reads every element of every buffer in one breadth-first sweep and
returns columns — `off hlen len tag cls cons par fc ns doc`, a node being an index into them. The
frontier starts as one position per buffer, each step parses the headers at all of them with
whole-vector operations (one gather for the identifier octet, one for the first length octet, four
for the long-form length, and `any` over the vector where the scalar reader had an `if` per
element), and a parsed element hands the next step both its first child and its next sibling, so the
frontier is wide almost at once: the 146-root system trust store is 9387 elements and derScan reads
it in 34 steps rather than 9387. `x509ParseMany` parses a whole bundle out of those columns, and
falls back to `x509Parse` for anything the columnar reader will not take — the high-tag-number form,
or any rejection at all — so the strict reader is still what decides every odd case and still
produces every message. `tests/x509.nt` is what says the speed was not bought by checking less: it
compares the two element for element and octet for octet over every certificate in the repository,
field for field over the same, and asserts that everything either must refuse, both still refuse.

It is a *batch* reader, and says so in its numbers: one
certificate costs 540 us through `x509ParseMany` against 485 through `x509Parse`, because derScan's
whole-vector steps have a fixed cost that one certificate does not amortise; three certificates — a
TLS chain — already come out ahead, 1295 us against 1520; 146 come out 2.6x ahead. `x509Parse` is
still the entry point for one, and still what `tls.nt` calls.

**What that cost, measured** (`tests/x509bench.nt`, `/etc/ssl/certs/ca-certificates.crt`, 146 roots,
155,984 octets of DER, 9387 elements, 64 to a certificate):

| | before | after |
| --- | --- | --- |
| PEM decode (`read0` + base64) | 128 ms | 5 ms |
| parse all 146 | 98 ms | 46 ms |
| **`x509LoadRoots`, end to end** | **231 ms — 1582 us/cert** | **54 ms — 370 us/cert** |

Most of the PEM win was one line: `pemDecode` split 215 KB with `"\n" vs`, which is 101 of those
128 ms. `read0` has already split the file into lines, so `pemLines` takes them as they came, finds
the BEGIN/END markers by line length (3610 lines down to ~320 candidates before the first `~`), and
decodes *every block's base64 in one pass* — each block is a whole number of 4-character groups, so
the groups of the concatenation are the groups of the blocks. It carries its own decoder rather than
calling `unb64` because `b64chars?s` is 2.6 ms over the store's 208 KB of base64 where a 256-entry
gather is 0.2 ms.

**Where the remaining 54 ms is**, and it is not where it was:

| | ms | us/cert |
| --- | --- | --- |
| `pemLines` | 5 | 34 |
| `derScan`, all 9387 elements | 1 | 7 |
| issuer and subject, RFC 2253 and canonical | 19 | 130 |
| extensions | 8 | 55 |
| SubjectPublicKeyInfo | 5 | 34 |
| validity, algorithm identifiers, the four byte slices | 5 | 34 |
| the per-certificate dict and loop around all of it | 11 | 76 |

**OpenSSL does the same work in 2.8 ms — 19 us a certificate — and this does not beat it.** The
walk is no longer the cost: it was 31 ms of the old 98 and it is 1 ms now. What is left is a floor
the language sets, and it is worth writing down exactly. Measured in this VM: an indexed read of an
int vector costs ~0.16 us, a call ~0.25 us, and a whole-vector operation on a short vector ~0.3 us,
dispatch and allocation rather than work. 19 us a certificate is about 70 of those operations, and a
certificate is 64 DER elements and 7 name attributes — so nothing written *per certificate* can fit,
whatever it does. Only a parser vectorised **across** certificates in every phase could, the way
`derScan` already is; the part that resists it is the name pipeline, where RFC 2253 escaping,
canonical lowercasing and whitespace collapse, and the joins are per-attribute string work.

The JIT would otherwise close that gap and cannot, for a reason worth recording. A scalar loop over
int vectors is exactly what the tiers want — an int-returning loop with nine parameters and three
nested levels compiles and runs at 10-20 ns an iteration against 300 interpreted. But **a function
that produces a vector is compiled by neither tier**: the same loop returning the buffer it filled
stays at 300 ns, and so does one that hands the buffer out through a global with `::` or through
`sset`. Parameters are by value, so a filled buffer has no other way out. Parsing is entirely
vector-producing, so none of it can be compiled — which is also why the three compiled name passes
written for this were removed again: correct, and slower than the vector code they replaced. A tier
that accepted a vector return would be worth more to this file than any further rewriting of it.

`src/neant/crypto/verify.nt` and `src/neant/crypto/tls.nt` (loadable, not in the boot image) are a
**TLS 1.3 client that authenticates the server** — x25519, `TLS_CHACHA20_POLY1305_SHA256`, and a
certificate path validator written on the files above:

```
load "src/neant/crypto/tlsclient.nt"      // the ten modules below, in dependency order
h: tlsConnect["www.google.com"; 443]      // verifies, or signals with the reason and closes
tlsSend[h; "GET / HTTP/1.0\r\nHost: www.google.com\r\nConnection: close\r\n\r\n"]
tlsRecv h                                 // one application-data record, 0x at end of stream
tlsClose h

x509CheckHost[cert; "a.example.com"]      // RFC 6125: SAN dNSNames and iPAddresses, never the CN
roots: x509LoadRoots "/etc/ssl/certs/ca-certificates.crt"        // 146 roots in ~54ms
x509VerifyChain[chain; roots; host; (now`date; now`time)]        // 1b, or signals why not
```

`tlsclient.nt` is one line per module and nothing else — `bignum`, `sha512`, `rsa`, `der`, `ec`,
`p256`, `p384`, `x509`, `verify`, `tls`, in dependency order. Ten in the right order is a list a
caller gets wrong before anything else, so there is one entry point for it; every module still
stands alone and still documents its own dependencies, and a program that wants only the hash or
only one curve should load exactly those.

`tlsConnect` parses the Certificate message, checks the server's CertificateVerify signature over
the handshake transcript (RFC 8446 4.4.3), verifies the chain to the trust store and matches the
hostname; any one failing closes the connection and signals. Verification is the default and the
opt-out — `tlsConnectOpts[host;port;(enlist `verify)!enlist 0b]` — has to be written at the call
site.

What `x509VerifyChain` checks, each with its own refusal message: the names chain, the signature
over every `tbs`, every validity window including the anchor's, basicConstraints `cA` and
`pathLenConstraint` on everything above the leaf, `keyUsage` `keyCertSign`, that no certificate
carries a critical extension the parser does not model, that the chain reaches the trust store, and
that the leaf covers the host — a wildcard only as the whole leftmost label, standing for exactly
one label.

Six signature algorithms can be checked, and that is the whole list: RSA-PKCS#1-v1_5 and RSA-PSS
over **SHA-256 or SHA-384**, **ECDSA-SHA256 on P-256** and **ECDSA-SHA384 on P-384**. The
ClientHello offers exactly those six schemes, the two ECDSA ones first. A chain that is any mixture
of them verifies end to end, chain signatures and CertificateVerify both — the P-256, P-384 and
SHA-384-RSA handshake tests each do that against a real `openssl s_server`.

**This reaches the public web now, and the README used to say — twice, in two different places —
that it did not.** The wall was a single link. A leaf that was ECDSA-SHA256 on P-256 verified; the
intermediate above it was `ecdsa-with-SHA384` signed by a P-384 key, for which this build had
neither the hash nor the curve, and that one refusal took out six hosts at once:

```
x509: CN=WE2,O=Google Trust Services,C=US is signed with ecdsaSha384 (1.2.840.10045.4.3.3),
which this build cannot verify
```

Measured from this checkout against the system trust store, wall clock so the network is in it
(`cargo test --release the_public_web -- --ignored --nocapture`):

| host | certs | handshake | the chain above the leaf |
| --- | --- | --- | --- |
| `www.google.com` | 3 | 0.45 s | ecdsaSha256/P-256, then ecdsaSha384/P-384 |
| `cloudflare.com` | 3 | 0.30 s | the same shape |
| `github.com` | 3 | 0.30 s | the same shape |
| `example.com` | 4 | 0.42 s | ecdsaSha256/P-256, then ecdsaSha384/P-384 twice |
| `www.wikipedia.org` | 4 | 0.71 s | ecdsaSha384/P-384 all the way up |
| `news.ycombinator.com` | 4 | 0.73 s | the same shape |
| `www.amazon.com` | 3 | 0.26 s | RSA-2048 PKCS#1-SHA256, which always verified |

All seven complete a verified handshake and return an HTTP response. The offline half of that is
`tests/data/chain-google.pem`, the capture that named the gap: both of its links verify, and the
whole chain verifies to the system trust store at a pinned `now` in ~62ms.

**What it costs.** One RSA-2048 verification is ~0.50ms, one P-256 ~21.5ms and one P-384 ~38ms, so a
two-link RSA chain is ~8ms, a one-link P-384 chain ~38ms and the Google chain — two ECDSA
signatures, one of each curve — ~62ms with the trust store already loaded. A process also pays
~54ms once for `x509SystemRoots[]` (it was ~231ms before the columnar reader), which `tlsRoots`
caches; that one is parsing, not arithmetic. Every signature number above was an order of magnitude
worse until the bignum arithmetic became compiled scalar loops (["the bignum arithmetic runs as
compiled scalar loops"](#the-bignum-arithmetic-runs-as-compiled-scalar-loops)); `p256.nt`'s header
counts out where what is left goes and which two optimisations were measured and rejected.

**extendedKeyUsage is enforced**, for `serverAuth`, and it nests: an intermediate restricted to
`emailProtection` cannot issue a server certificate under it, so a certificate a CA correctly limited
to S/MIME or code signing cannot serve a web request just because its SAN carries a hostname. RFC 5280
defines the extension per certificate and says nothing about chaining; the nesting is what CA/Browser
Forum requires and what browsers do, and it costs a real chain nothing — Google's leaf carries
`serverAuth` and both certificates above it carry `serverAuth` and `clientAuth`. An **absent**
extension is unconstrained, per 5280, and a trust anchor is exempt: it is trusted by being in the
store, not by what it says about itself. The refusal names the OIDs the certificate does carry and
the one it does not.

**Revocation** is `src/neant/crypto/crl.nt`: parse a CRL, verify its signature with the issuing CA's
key, and ask whether a serial is on it. OCSP is deliberately absent — Google's certificates no longer
carry an OCSP responder at all, only a CRL distribution point, which is what settled it. Fetching is
the *caller's*, also deliberately: `x509VerifyChain` is pure, and a verifier that reached for the
network mid-chain would turn every verification into something that can hang and would leak which
certificate is being checked to whoever runs the distribution point. So `crlUrls` says where the list
lives, `src/neant/net/http.nt` fetches it, and the program composes the two and gets to decide about
timeouts, caching and what to do when the fetch fails.

```
load "src/neant/crypto/crl.nt"; load "src/neant/net/http.nt"
h: tlsConnectOpts["example.com"; 443; (enlist `crl)!enlist {[u] (httpGet u)`body}]
```

`crl` is opt-in and is the *fetcher* rather than a flag, so the caller owns the timeout, the caching
and what an unreachable distribution point means. Nothing soft-fails: a fetch that does not come back
takes the connection down, because "could not check" is not "checked and it is fine".

**It checks the intermediates, not the leaf**, and the reason is a measurement. A public CA's leaf
CRL is enormous — example.com's is **43,495,327 bytes and 887,654 serials**, 3.0s to fetch and 12.8s
to parse (`derScan` is 8.1s on the same buffer, so it is the size and not the parser). Its two
intermediates' lists are **301 and 299 bytes and 1ms**. Checking the intermediates costs **36ms** on a
313ms handshake and catches the case revocation is really for, a sub-CA that has to be withdrawn;
checking the leaf as well turns a 313ms handshake into 18.5s. That asymmetry is the whole story of
why the web left CRLs for OCSP and then for pushed lists, and no parser fixes it. `crlFull` is the
complete check for when you want it — a private PKI, a list you fetched once and cached, or a leaf
whose CA publishes something reasonable.

Against the real web (`tests/data/live-crl.nt`): Google's leaf names
`http://c.pki.goog/we2/Gt0Gl6QoGAU.crl`, which is 53,504 bytes fetched in ~400ms, parses in 17ms to
1499 revoked serials, and its ECDSA-SHA256 signature verifies against the intermediate in 34ms. A
serial the list carries is found; the live leaf is not on it. Not implemented: delta CRLs, indirect
CRLs (a list signed by anyone but the certificate's own CA is refused rather than guessed at),
reason codes, and `issuingDistributionPoint` — a CRL carrying a critical extension this does not
model is refused, on the principle `x509.nt` already applies. A stale list is refused rather than
believed; a list with no `nextUpdate` is not treated as stale, since 5280 only says it SHOULD be
there and refusing every list that omits it would refuse more than it protects.

**Signing** is `src/neant/crypto/sign.nt`, kept apart from `rsa.nt` on purpose: verifying touches
nothing secret, signing touches `d`, and a program that only verifies — which is every TLS client
here — should never load the code that reads a private key.

```
load "src/neant/crypto/sign.nt"
k: rsaKeyLoad "key.pem"                      // PKCS#8 or PKCS#1, and p*q is checked against n
sig: rsaSignPss[k; `sha256; sha256 msg]      // salt from urand; rsaSignPssSalt fixes it for a vector
sig: rsaSignPkcs1[k; `sha256; sha256 msg]    // the deterministic v1.5 form
```

RSASSA-PSS and PKCS#1 v1.5, over any hash `rsa.nt`'s table knows, ~19ms for a 2048-bit key by CRT.
**ECDSA** on P-256 is there too, with the nonce from **RFC 6979** rather than from a random source:

```
k: ecKeyLoad "ec-key.pem"
sig: ecdsaSignDer[P256C; k`d; sha256 msg]     // the DER SEQUENCE { r, s } TLS and X.509 carry
```

ECDSA's nonce is not a salt. If it repeats across two signatures, or is biased, or leaks a few bits,
the private key falls out by algebra — that has taken real keys, from the PS3 to Bitcoin wallets.
Deriving it from the key and the message with HMAC-DRBG removes the random source as a failure mode
entirely, and it makes the standard's own test vectors pin the implementation: `tests/sign.nt`
checks RFC 6979 A.2.5's **k, r and s**, not just that a signature verifies.

The oracle is OpenSSL: every pinned signature in `tests/sign.nt`, RSA and ECDSA alike, was produced
by this code and then checked with `openssl dgst -verify`, so the test freezes that agreement without
needing openssl to run. **Neither signing path branches on a secret any more** — the ECDSA scalar
multiply is `ecCtMul` and the RSA exponentiation is `bnModExpCtL` over a blinded exponent — but
neither blinds the base, which is the setting where an attacker picks the messages. ["What has been
moved onto it"](#what-has-been-moved-onto-it) is exact about where that line falls.

RSA over SHA-512 (PKCS#1 and PSS) and Ed25519 in a chain are checked as of `tests/data/pki-ed-gen.sh`'s
fixtures. Ed25519 is the one algorithm here that names no digest: RFC 8032 signs the message and
hashes it internally, so `verify.nt` hands `ed25519Verify` the tbs span rather than a digest of it.
It needs `ed25519.nt`, which `tlsclient.nt` therefore loads — 21ms on a 542ms load, chosen over
dispatching on whether the global happens to exist, which would have made a verifier's answer depend
on what else the program had loaded.

What is still **not** checked, plainly: **P-521**, refused with the curve named, because there is no
`p521.nt`; **ecdsaSha512**, refused for the curve-pairing reason below rather than for want of the
hash; **SHA-1**, refused because it is broken; **name constraints** and certificate
policies. There are **no client certificates**: the server side (below) never sends a
CertificateRequest, so whoever connects to it is anonymous. A mismatched
ECDSA algorithm and curve — `ecdsaSha384` under a P-256 key, or the reverse — is also refused, with
both named. Every one of those refusals names the algorithm or the curve rather than skipping the
check. This is a verifier written from scratch to be read, not a substitute for a reviewed TLS
stack. Verification has no secrets to leak; what the signing side does and does not hide is below.

