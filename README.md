# neant

A language where what the machine code does is a property the compiler checks, not a hope.

In every other language, "this function must be constant-time", "this function must not allocate",
"this function must stay compiled" are comments and code review, and a profile long afterwards. Here
they are questions with answers, asked of the compiler from inside the language, about the machine
code it actually emitted.

```
jitct  {[a;b] (a band b) bor a bxor b}   // ""   no data-dependent branch in the emitted code
jitct  {[a;b] (a bxor b) + 1}            // the check is back, and named
jitwhy {[x] w: (); w,: x; w}             // "does not compile: op 0 (list) is not in the subset"
jitwhy {[a;b] (a bxor b) + 1}            // "compiles, but can fall back ... 2 x an int-null check"
```

`jitct` compiles a function in exactly the environment the JIT compiles it in, then **decodes the
machine code it emitted** and answers `""` only when there is no data-dependent control flow in it.
It is a check on the bytes, not an argument about the source, so it cannot be talked around.

## Why this is possible here and not elsewhere

Every other tool that makes a machine-level claim has to trust the compiler underneath it.
Cryptol and SAW check a specification, HACL\* checks F\* source and then hands the result to a C
compiler that is free to reintroduce a branch, Jasmin checks its own IR. The gap between what was
verified and what ships is where the property gets lost, and "the optimiser turned my mask back
into a branch" is a real and recurring way to lose it.

neant closes that gap by being small enough to look at the other end. The backend is
`src/neant/jit/arm64.nt`, written in neant; the checker reads what it emitted. A large compiler
cannot credibly make this claim. That is the whole of the advantage, and it is structural: matching
it means writing a small compiler, not adding a pass.

```
runtime      4,515 lines of Rust, zero dependencies
front end      516 lines of neant, self-hosting
backend      2,746 lines of neant
```

## The evidence it works

**A TLS 1.3 client, in neant, that reaches the public web.** DER, X.509, RSA over SHA-256 and
SHA-384, ECDSA on P-256 and P-384, a trust store, chain validation and hostname matching — Google,
Cloudflare, GitHub, Wikipedia and Amazon all complete a verified handshake. Every hot kernel is a
scalar loop the whole-function JIT compiles: SHA-256 **1.0ms** per 64KB, ChaCha20 **1.7ms**,
Poly1305 **0.34ms**, X25519 **3.0ms**. ([docs/crypto.md](docs/crypto.md))

**Four secret-dependent branches found and removed, with the cost of each measured.** Not argued —
measured, on scalars chosen to make the branch fire or not:

```
                    1 bit set    255 bits set
ecShamir               218 ms         455 ms     <- the side channel, in one number
ecCtMul                592 ms         589 ms

bnModExpL              171 ms         306 ms
bnModExpCtL            406 ms         405 ms
```

Plus a tag comparison that returned early and an x25519 swap that branched on two adjacent key bits.
Each is asserted constant-time in `tests/ct.nt`, so a helper rewritten with an `if` fails the suite
rather than quietly costing the property. ([docs/constant-time.md](docs/constant-time.md))

**The property survives contact with real code.** `src/neant/stdlib/ct.nt` is the set of primitives
built on it — `ctMask`, `ctSel`, `ctEqBit`, `ctLtBit`, `ctAcc` — each written to a discipline the
compiler can see rather than a mode a caller has to remember.

## The plan: ask the compiler

The first draft of this section proposed declarations — `where compiled`, `where ct` — that a
function carries and the build enforces. That was wrong twice over, and the reasons are worth
keeping.

A declaration only protects what someone thought to declare. docs/compiler.md's SHA-512 lost ninety
times its speed to a single `w,:`, and nobody would have annotated it, because nobody knew there was
anything to annotate. And for a *performance* property, failing the build is the wrong severity: the
annotation is the first thing deleted when it gets in the way. Worse, for constant time the contract
already exists and needs no syntax at all — `tests/ct.nt`'s `ctIs` asserts `jitct f` is `""` for
each primitive, and a `where` clause would add locality and nothing else.

What is actually distinctive here is smaller and already half-built:

> **How a function compiled is a value the program can ask for.**

`jitct f` and `jitwhy f` are ordinary functions returning ordinary values. Every other JIT keeps this
behind an out-of-band log flag — `-XX:+PrintCompilation`, `--trace-deopt` — that a human reads after
the fact. None of them lets the running program ask. Once it is a value, the policy is written by
whoever is using it rather than fixed by the language: a crypto file signals on a bad answer, a hot
loop only prints one, and neither needs a keyword.

### 1. `jitwhy`, structured — done as prose, wanted as data

`jitwhy f` answers "" when `f` compiles and holds no path back to the interpreter, and otherwise why
not. Today that answer is a sentence, which only a person can act on. The same facts as a dict —
whether it compiled, how many ways out and of which kind — let neant code act on them, with the
sentence kept as a formatter over the top.

### 2. Enumerating what to ask about

The names bound in the global namespace are not reachable from neant today (`vm.rs`'s `slot_names`
holds them). One primitive exposes them, and it is the only part of this that needs Rust.

### 3. The sweep, in neant

With those two, walking every bound function and reporting the ones that cannot compile — or that
compile and can still leave — is a dozen lines of neant, and so is every policy built on it:

```
if[0<count jitscan[]; signal "a kernel fell out of the compilable subset"]   // strict
show jitscan[]                                                              // advisory
```

This is what would have caught SHA-512 without anyone anticipating it, and it is the same tool that
answers the open question on the constant-time side: not "does this function pass `jitct`" — `ctIs`
already asks that — but **which functions handle a secret and were never asked at all**.

### 4. The property itself is still incomplete

No syntax closes this one. `jitct` checks branches and **not memory addresses**, and a
secret-dependent load is the other half of constant time — the half that broke AES T-tables.
`src/neant/stdlib/ct.nt` avoids it by never indexing, and avoidance is a discipline rather than a
check. Tainting secret values through registers and looking at the address operand of every load and
store is the real work here, and it also lifts the opposite limitation: with addresses checked, a
bounds test on a *public* index no longer has to be refused.

### 5. The language debts underneath all of it

Three decisions tax every one of these, and each shows up as a line in `ct.nt`'s list of things its
code may not do:

- **The int null is a bit pattern** (`0x8000000000000000`), so `+`, `-` and `*` emit a check against
  it, which is a branch. That is why `badd` exists as a second `+`, why SHA-512's kernel contains no
  arithmetic operator, and why `bnMontFinL` cannot be checked at all. A `u64` width — a type with no
  null — deletes the check, the workaround and the gap together. **Highest ratio of anything here.**
- **A vector slot is `Ints` and nothing else**, so a byte or char buffer stops a function compiling.
  That is certificate parsing, base64 and the formatter, all of them unreachable by any of this.
- **A compiled function calling another goes through a trampoline whose bail-out check is a branch**,
  so `ct.nt` is written out rather than composed. Proving a callee cannot deopt — which `jitwhy`
  now answers for a single function — makes the call direct and makes the property compose.

One silent failure is already fixed: a bare `-1` after a verb lexes as monadic minus and falls out of
the compilable subset, so a missing pair of parentheses used to drop the guarantee without a word.
`jitwhy` says so now.

## Run

```
cargo run --release                     # REPL
cargo run --release -- file.nt          # run a file
cargo test --release
```

After editing `src/neant/{core,stdlib,crypto}/*.nt`: `cargo run --release -- --build-boot`, then
rebuild — `cargo test` fails until the embedded `src/neant/image.nb` matches the sources again. A
change to the *compiler* needs the cycle twice: the first pass compiles the new compiler with the
old one, the second is the fixpoint the test checks for.

The image is the only front end, so it is also the only seed. A `src/neant/image.nb` that cannot
compile its own sources can only be rebuilt by a binary that still carries a working one — the last
good build, or the copy in git. Removing that dependency, so the tree builds from source alone, is
on the list above's other side: it costs nothing a user sees and buys everything a stranger needs to
trust the build.

## The language, briefly

A vector language in the kdb/q family — that is where the notation came from, not where it is going.
The array tier earns its place as the form a specification is written in: short enough to read once
and check, which is how `bnMulV` serves as the oracle that `bnMulL` is tested against. Production
code is scalar loops the compiler can take.

```
x: 1 2 3 4          // vector literal, assignment
2*x+1               // 4 6 8 10      no precedence, strict right-to-left
+/x                 // 10
{x*y}[3;4]          // 12            lambdas, implicit args x y z
f: +/               // a verb is a value; f 1 2 3 -> 6
```

Full reference: [docs/language.md](docs/language.md).

## Documentation

| | |
|---|---|
| [docs/language.md](docs/language.md) | syntax, types, standard library, builtins, errors, gotchas |
| [docs/library.md](docs/library.md) | tables, JSON, regexes, formatter and LSP, HTTP, TLS server, concurrency |
| [docs/compiler.md](docs/compiler.md) | how it is built, what the JIT compiles, how that is tested |
| [docs/constant-time.md](docs/constant-time.md) | `jitct`, and the four branches removed with it |
| [docs/crypto.md](docs/crypto.md) | what is implemented on the bytes, against which vectors |
| [docs/performance.md](docs/performance.md) | what was measured and what it bought |

## What this is not

`jitct` proves one property and not constant time in general: `MUL` is constant-time on the cores
this targets without being architecturally required to be, and nothing here says anything about what
a caller does before or after. The other three contracts are not built. The measured numbers come
from one machine and should be read as ratios.

What is claimed is narrower and, it is hoped, more useful: that a compiler small enough to be read
can be made to answer for its own output, and that this is worth more than a larger compiler's
silence.
