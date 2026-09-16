# neant

A vector language in the kdb/q family, meant to grow into a general-purpose one.

- **No operator precedence, strict right-to-left.** `2*3+4` is `14`. The parser is one linear
  pass with no precedence table and no backtracking — that is where the compile speed comes from.
- **Bytecode VM over typed vectors.** `1 2 3` is a `Vec<i64>`, not a list of boxed values; folds
  like `+/x` fuse into a single native loop.
- **Verbs are single characters, names are values.** `f: +/` then `f 1 2 3` → `6`.

```
x: 1 2 3 4          // vector literal, assignment
2*x+1               // 4 6 8 10
+/x                 // 10
{x*y}[3;4]          // 12          lambdas, implicit args x y z
f: {[a;b] a-b}      //             or named params
$[1<2;`yes;`no]     // `yes        cond
fact: {$[x<2;1;x*fact x-1]}
d: `a`b!1 2; d`b    // 2           dicts
x[1]: 9; d[`c]: 3   //             index assignment (in place), n+:1  x,:5
f[;3] 10            //             projection: f[;3] fixes b, waits for a
{x*2} each 1 2 3    // 2 4 6       also `over` `scan`; `(+) over x`
@[{signal "boom"};0;{"caught: ",x}]   // protected call: @[f;x;handler]
{if[x<0; :`neg]; `pos}              // :x returns early from a lambda
counter::0; {counter::counter+1}[]  // :: assigns a global from inside a lambda
1 -2                // vector 1 -2   (glued minus is a literal; `1 - 2` subtracts)
```

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
good build, or the copy in git.

## The language

### Verbs, adverbs, control

- **Verbs** — `+ - * % & | < > = ~ , # _ ! ? @ ^ $`, each monadic and dyadic, q meanings.
- **Adverbs** — `/` fold, `\` scan, `'` each, `\:` each-left, `/:` each-right.
- **Control** — `if[c;...]` `while[c;...]` `do[n;...]` `$[c;a;b;...]`; `break` leaves the innermost
  loop, `:x` returns from the lambda.

### Literals and types

```
101b  0N 0W  0n 0w                 // bool literals; int null/infinity (0N propagates through + - *); float null/inf
2026.09.15 + 30                    // dates (days since 2000.01.01): 2026.10.15;  d1-d2 -> days;  `year$ `month$ `day$
12:30:00.250 + 1000                // times (ms since midnight);  `hour$ `minute$ `second$;  today[]  now`time
2026.01.01 2026.01.03              // date vector literal
`date$"2026.02.28"  `int$d         // casts both ways; isnull x; fill[0;x]; fills x
0x0aff                             // bytes — see below
```

### Assignment, scope, control

```
x[1;0]: 9   c[1]+: 10   do[5; ..]  // deep index assignment, compound index assignment, do loop
f: {n: 10; {x+n}}; (f 0) 5         // closures capture enclosing locals by value -> 15
1 2 3 +\: 10 20   1 2 3 +/: 10 20  // each-left / each-right
.ns.name: 7                        // dotted names as namespaces; load "file.nt" runs a file
```

### Standard library

Written in neant, in `src/neant/stdlib/prelude.nt`:

| Group | Functions |
|---|---|
| Aggregate | `sum avg min max med var dev count any all` |
| Math | `sqrt floor ceiling round signum neg mod div xexp` |
| Lists | `til first last reverse raze sort asc desc distinct where rank rotate cut sublist except inter union cross` |
| Running, windowed | `sums prds maxs mins deltas ratios prev next differ msum mavg mmax mmin ema xbar bin` |
| Strings | `string sym vs sv ss upper lower trim ltrim rtrim ssr like fmt hex unhex` |
| Tests | `type not in within` |

### Rust builtins

Only what needs the host; everything expressible with the verbs lives in the prelude instead:

| Group | Builtins |
|---|---|
| Math | `exp log sin cos tan atan` |
| Random | `rand rseed` — `n rand m` draws n from `[0;m)` or from the list m, `rseed 7` makes a run reproducible. `urand n` is n bytes from the OS: `rand` is a PRNG seeded from the clock, so keys come from `urand` |
| Bits | `badd band bor bxor shl shr bnot` — on the raw 64-bit pattern; `badd` is `+` without the int-null case, for u64 words |
| Dicts | `key value group` |
| Values | `isnull now` |
| Output | `show print signal exit` |
| Files | `read0 write0` |
| Sockets | `hopen hclose hsend hrecv hlisten accept` |
| Concurrency | `spawn join shared sget sset supd` — see [Concurrency](#concurrency) |
| Adverb keywords | `each over scan` |
| Errors | `elast` |

```
read0 "f.txt"                      // list of lines; read0 0 reads stdin
args                               // command-line arguments after the script (a global, not a builtin)
h: hopen "example.com:80"          // TCP; hopen ("host:port"; timeoutMs) sets the timeout
hsend[h; "GET / HTTP/1.0\r\n\r\n"]
hrecv[h; 4096]                     // one read, up to n bytes; empty means the peer closed
hclose h

l: hlisten "0.0.0.0:8080"          // a listener is a handle too — hclose works on it unchanged
while[1; c: accept l; spawn {hsend[c; "hi\r\n"]; hclose c}]   // one spawned worker per connection
```

`accept` blocks for the next inbound connection and returns an ordinary connection handle — capture
it into a `spawn`ed closure (src/vm.rs) and `hsend`/`hrecv`/`hclose` on it there exactly as if it
came from `hopen`. This is why socket handles (`src/prims.rs`) live in one global table behind a
lock rather than a thread-local one: the accepting thread and the worker handling the connection are
different OS threads, and both need to resolve the same handle. That lock is held only for the
lookup, never across the actual blocking read/write/accept (each clones the underlying file
descriptor and blocks on the clone) — otherwise one worker still waiting on a slow client would
stall every other socket in the process, exactly the concurrency this is for. No TLS server side yet
— `src/neant/crypto/tls.nt`'s handshake code is still client-only.

## Tables

`src/neant/stdlib/table.nt`, in neant — a table is a dict of columns.

```
t: tbl[`name`dept`pay; (`ann`bob`cy; `eng`ops`eng; 120 80 100)]
tsel[t; t[`pay]>90]          // rows where
tby[t;`dept;`pay;sum]        // `eng`ops!220 80
tshow tsort[t;`pay]          // aligned grid
```

| Group | Functions |
|---|---|
| Build, show | `tbl row rows tcount tappend tshow` |
| Query | `tsel tsort tby xasc xdesc ungroup` |
| Joins | `lj ij uj aj` |
| Keyed | `xkey unkey` |
| CSV | `rcsv["SSI";",";"file.csv"]` |

```
select total: sum pay, n: count pay by dept from t where pay>90
                             // q-style select, desugared by the parser into qsel[t;where;by;cols]
t[1]  t[0 2]                 // rows by position; t[where t[`pay]>90]
kt: xkey[`id;t]; kt 3        // keyed table: key rows -> remaining columns; kt[(1;2)] for a multi-column key
(1 2;3 4)?3 4                // ? on a general list finds a whole row (1); so does `in`
```

## JSON

`src/neant/stdlib/json.nt`: `jk` parses (objects are dicts, null is `::`), `jj` serializes.

```
jk "{\"a\": [1, 2]}"         // ,`a!(1 2)
jj `a`b!(1 2;"x")            // "{"a": [1, 2], "b": "x"}"
```

## HTTP

`src/neant/net/http.nt` (loadable, not in the boot image): `httpRecv`/`httpSend` parse a request and write a
response over an `hopen`/`accept` handle; `httpServe` wraps the accept-loop-plus-`spawn` pattern shown earlier (`hlisten`/`accept`, "Rust
builtins" above) into one call.

```
load "src/neant/net/http.nt"
l: hlisten "0.0.0.0:8080"
httpServe[l; {[req] (200; "OK"; (`$"content-type")!(,"text/plain"); "you asked for ",req[`path])}]
```

`req` is `` `method`path`version`headers`body!(...) ``, headers keyed by lowercased symbol (build
one with `` `$"content-length" ``, not a literal `` `content-length `` — a hyphen in a *literal*
symbol token is the `-` verb, not part of the name; casting a string with `` `$ `` has no such
limit). A handler returns `(status; reason; headers; body)`. No chunked transfer-encoding, no
keep-alive (`hclose` after every response), no URL/query decoding, no HTTPS yet — `src/neant/crypto/tls.nt` is
still client-only.

## Bytes and crypto

```
0x0aff                       // byte literal
`byte$"hé"                   // 0x68c3a9   UTF-8 encode
`char$0x68c3a9               // "hé"       decode
`int$0x0aff                  // 10 255     arithmetic on bytes gives ints
key bxor data                // the bit verbs on two byte operands give bytes
```

`src/neant/crypto/crypto.nt` is pure neant on those: `sha256 hmac hkdfExtract hkdfExpand chacha20 poly1305
aeadEncrypt aeadDecrypt x25519`, all checked against the RFC vectors (SHA-256 ~0.12ms/block,
ChaCha20-Poly1305 ~16ms per 10KB, X25519 ~42ms). 32-bit words live in ints masked after each sum;
the 2^255-19 and 2^130-5 fields use 22- and 26-bit limbs so products stay exact in an int, and
carries run as vector passes.

`src/neant/crypto/ed25519.nt` (loadable, not in the boot image) adds **SHA-512 and Ed25519 verification** on top
of that field — RFC 8032 vectors, ~68ms per signature:

```
load "src/neant/crypto/ed25519.nt"
ed25519Verify[pub; msg; sig]       // 32-byte key, 64-byte signature -> 1b / 0b
hex sha512 `byte$"abc"
```

64-bit words need no splitting: `band bor bxor shl shr` are exact on the raw `i64` pattern (`shr` is
logical, `shl` discards), and `badd` is the wrapping add — plain `+` would read `1 shl 63` as an int
null and poison the round. The curve reuses `fadd fsub fmul fsq finv fencode fdecode` unchanged, in
extended coordinates with the complete addition law, and Shamir's trick does both scalars in one pass
of 253 doublings. Scalars reduce mod L one bit at a time. Verification only: no signing, and nothing
is constant-time, which is what a verifier's all-public inputs allow.

## Errors

Lexer and parser errors carry the line. A runtime error points at the line that actually failed and
unwinds a named call stack:

```
f: {x+`a}
g: {f x}
g 1
'type: arithmetic on non-numeric at line 1
  in f at line 1
  in g at line 2
  at line 3
```

`@[f;x;handler]` catches; inside the handler, ``elast `line`` and ``elast `trace`` say where the error
came from.

## Concurrency

```
n: 10; h: spawn {n+1}; join h        // 11 — a niladic closure on its own OS thread; join blocks for the result
s: shared 0                          // an opt-in mutable cell; every other value stays lock-free
supd[s; {x+1}]; sget s               // 1 — atomic read-modify-write: locks s for the whole call to the function
hs: {spawn {work n}} each til 4      // one shared VM snapshot forked to four threads
{join x} each hs
```

`Value` is `Arc`-refcounted and copy-on-write, the same discipline `x[i]:v`/`n+:1` already use for
in-place amend (mutate only when uniquely owned, else copy) — so ordinary values cross threads for
free, with no lock and no global interpreter lock serializing them. `spawn f` runs a niladic `f` on a
new OS thread with its own VM, forked from a snapshot of the caller's globals at the moment of the
call; `join h` blocks for the result, or re-raises an error signalled inside `f`, or errors if `h` was
already joined. The one exception to lock-free is `shared x`, an explicit mutable cell: `sget`/`sset`
read and overwrite it, but only `supd[s;f]` is atomic — it holds the lock for the whole call to `f`, so
concurrent `supd`s on the same cell serialize instead of losing an update the way `sset[s; f sget s]`
would if two threads interleaved between the get and the set.

## Gotchas (shared with q)

- `i+1<n` is `i+(1<n)`. Write `(i+1)<n`. Every comparison inside arithmetic needs parens.
- `string +/v` is `+/` applied dyadically to `string` and `v`. Write `string sum v` or `string (+/)v`.
- A glued `-` after a noun is subtraction: `f -1` is `f - 1`; write `f[-1]`.
- A name followed by a verb is that verb's left argument: `til #p` is `til # p` (take), `value =x` is `value = x`. Write `til count p`, `value group x`. When the name holds a *function* this used to build a silent two-element list — `f ,x` is now an error that says so.
- `in` against a plain string is per character: `"ab" in "abc"` is `11b`, not a substring test — use `ss` for that. Against a *list* of strings it does match whole strings, so `"from" in ("by";"from")` is `1b`.
- Closures capture by value; assigning a captured name inside the inner lambda makes it a new local (like q). No mutable counters.
- A variable assigned anywhere in a lambda is local to it. `x::v` assigns the global. A local cannot be named after an infix verb (`in`, `sv`, `cut`, `bin`, …) — the parser would read it as the verb, so the compiler rejects it by name.
- A newline ends a statement, so an expression cannot be split across lines. Build it up with `,:` instead.

## How it is built

### Stage 0 (done)

Lexer, parser, compiler and VM in Rust.

### Stage 1 (done)

lex/parse/compile rewritten in neant and running on that VM, PyPy-style; the Rust VM and primitives
stay as the runtime. The Rust front end has been deleted — `src/` is `{vm, prims, value, image, jit,
trace, main}.rs` plus `src/neant/` (the self-hosted sources, below, and the compiled image), and **source
never reaches Rust**.

- `src/neant/core/lex.nt` — the lexer.
- `src/neant/core/parse.nt` — the parser. Nodes are ``(`kind; ...)`` lists with identifiers as symbols.
- `src/neant/core/compile.nt` — the compiler. It emits bytecode as data: a unit is
  `(opcodes; args; consts; lines)`, where `lines[i]` is the source line op `i` came from (0 for
  synthetic ops). Consts are tagged ``(`k;v)`` ``(`g;`name)`` ``(`p;"+")`` ``(`a;"/";f)`` ``(`f;code)``.
  `exec` loads and runs it.
- `nrun src` is the whole pipeline; `load "f.nt"` is `nrun` over a file. The REPL and the file runner
  are both one `nrun` call.

`--build-boot` compiles `src/neant/{core,stdlib,crypto}/*.nt` **with the compiler already in the image** and serializes the
bytecode into `src/neant/image.nb` (`src/image.rs`), embedded in the binary by `include_bytes!`. Rust is the
VM plus the primitives, and nothing else.

`src/neant/` is grouped by what a file is for, not by whether it ends up in the image — `core/`
(lex/parse/compile, above) and `stdlib/` (prelude/table/json) do; `crypto/` is split between what's
in it (`crypto.nt`) and what's loaded on demand (`ed25519.nt`, `tls.nt` — see their own sections);
`net/` (`http.nt`) is loadable only. `load "f.nt"` doesn't care which directory a file is under.

### Stage 2 (started): a baseline JIT for integer loops and calls

The codegen itself — the compilability walk over `Op`s and every AArch64 instruction encoder — is
self-hosted the same way Stage 1 is: `src/neant/jit/arm64.nt` is a neant function, `jitCompile`,
that takes a `FnCode`'s bytecode as data (`(opcodes;args;consts;arity;nlocals)`,
`FnCode::jit_input`) and returns either a `Bytes` value (the encoded machine code) or `::` (not
compilable), the same null-signals-failure convention used elsewhere. Only what genuinely needs the
host stays in Rust (`src/jit.rs`, AArch64 only, no crate dependency): `mmap`/`mprotect`/
`__clear_cache` declared directly as `extern "C"` (already linked in via libc/libgcc), owning the
resulting executable memory (`Compiled`), and the `jit_call` trampoline that a compiled function
uses to call another one directly. After 64 calls a `FnCode` is walked once by `jitCompile`, and if
every op in it is provably pure integer arithmetic over its own locals — `+ - * & | < > =`,
`Push`/`LoadL`/`StoreL`/`Pop`/`Ret`, `Jmp`/`Jmpf`/`Loop` (`while`/`if`/`do` control flow), and a
call to another function that is *itself* provably pure the same way — it's compiled to native code
and the result cached on the function (`FnCode::jitted`, `src/value.rs`); anything else (a global
that isn't an immediately-called function, a closure, a float, a call to something impure) is
rejected once and runs interpreted forever after, same as always. This follows the atomic-swap-in
discipline [Concurrency](#concurrency) already committed to: `intern()` (`src/vm.rs`) patches a
`FnCode`'s consts in place today via the same "mutate only when uniquely owned" check `x[i]:v`
uses, which the JIT doesn't reuse for compiled code — a compiled function is built complete, then
stored once behind a lock (`OnceLock`), never edited in place, so two threads racing to compile the
same hot function just waste one's work instead of racing on it.

Compiling now means *running* neant code (`Vm::call`), which needs `&mut Vm` — sound to reborrow
even from inside the `jit_call` trampoline (which only ever holds a raw `*mut Vm`) under the same
discipline every other reentrant interpreter-calls-host-calls-interpreter path already relies on
(adverbs like `each` call back into `self.call` the same way): single-threaded, strictly nested,
the outer call's own `&mut self` untouched while the nested one runs. One wrinkle unique to a JIT
that compiles itself: `jitCompile` and its own helpers are neant functions too, and their own
bytecode contains the very op kinds they exist to handle — a `Dyad` inside the code that handles
`Op::Dyad`, for instance. Once one of those crosses the same 64-call threshold, compiling it would
mean *calling* it to walk its own bytecode, reentering its own `OnceLock` from inside that same
`OnceLock`'s initializer. A thread-local guard (`already_compiling`, `src/jit.rs`) shuts that off
for the whole nested call, not just the one function that would recurse — the JIT's own
implementation is never a JIT target, full stop, which only ever affects how long one-time
compilation takes, never a compiled function's own speed.

`Loop` (`do[n;..]`) is the one op whose two edges leave the interpreter's stack at different depths
— decrementing in place and falling through to the body leaves it unchanged, but exiting also pops
— so it's the one place the compiler can't just accumulate stack depth linearly through the
bytecode; the exit edge's depth is recorded and used to override that accumulation when the scan
reaches it, which also makes nested `do` loops compile correctly.

A compiled function is guarded at entry (every arg and capture must be a plain, non-null int) and
can still bail out mid-run back to the interpreter (`deopt`) at a few points it can't just trust
blindly: two ordinary ints wrapping to exactly the null sentinel by coincidence (`0W+1`); a call
whose arity doesn't match or whose callee turns out not to be a plain function; runaway recursion
(compiled-to-compiled calls go through `blr`, not `Vm::call_code`'s own recursion-depth guard, so
they need their own, `MAX_CALL_DEPTH` — lower than `call_code`'s `self.depth` limit, since each
level here carries a `[i64; MAX_SLOTS]` stack buffer (see below) that a plain interpreted call
doesn't, and both are calibrated empirically against the smallest stack this can run on, not just
the main thread's, since a `spawn`ed thread defaults to a 2MiB one). Since the compilable subset
can't observe anything outside its own locals — and a call is only made at all once the callee is
independently proven just as pure, by literally attempting to compile it too — every one of these
is always safe to just re-run from scratch on the interpreter.

Calling another compiled function goes through one fixed trampoline (`jit_call`) reached via `blr`:
it resolves the callee exactly like `Op::LoadG` would, proves it pure (or refuses to call it at all
if not — the only way to avoid firing a real side effect twice if the *caller* later deopts), and
recurses through compiled code directly, never dropping back into the bytecode interpreter unless
something along the way deopts. The callee's compiled version is cached on its own `FnCode` behind
a `OnceLock` (`FnCode::jit_for_call`, `src/value.rs`), not a mutex — built at most once, read with a
plain atomic load on every call after that, since this runs on every single recursive step. Its
locals go on a fixed-size native stack buffer (`try_run_raw`, `src/jit.rs`) instead of a heap
`Vec`, since the arguments are already known to be plain ints (they came from another compiled
function's own int-typed registers) and real functions have nowhere near `MAX_SLOTS` (64) locals —
between the two, a hot recursive call pays neither a lock nor an allocation.

**Locals live in registers.** That buffer (and `try_run`'s) is only how a call's locals get *in*.
Every local a compiled function touches — params, scratch locals and captures alike, in first-seen
bytecode order (`jitLocalSlots`, `src/neant/jit/arm64.nt`) — is assigned one register for the whole
function, loaded from the buffer once in the prologue; `LoadL`/`StoreL` compile to register moves,
and the values are never written back out. That's sound because `Compiled::run` reads only the
returned value and the ok word, and neither entry point looks at the buffer again after the call, so
a return or a deopt simply abandons the registers; and both entry paths marshal into the same layout
and reach the same entry, so one prologue serves both. What makes this different from the tracing
tier's version of the same idea ([Stage 2b](#stage-2b-started-a-tracing-jit-for-hot-loops)) is that a
compiled function makes calls — `blr` to the `jit_call`/`jit_vec_get`/`jit_vec_set` trampolines — and
its locals have to survive them. So the first six registers handed out are callee-saved (x23..x28),
which the trampolines preserve for free at the cost of one `stp`/`ldp` per pair in the prologue/
epilogue, paid once per call *of* the function and only for the pairs in use; the next five are
caller-saved (x6..x8, x16, x17) and are spilled to the frame around every `blr`, the same way the live
operand stack already was — a cost only a function with more than six locals *and* a call in it pays.
A twelfth local and beyond simply stays in the buffer and is reached through x19 exactly as every
local used to be, per slot, so no function is rejected for being too wide; its overflow locals just
run the old way. A vector-classified slot (below) is a register too — its value is a pointer, a plain
64-bit word only ever handed to the vector trampolines — so those keep their semantics unchanged.
The frame grew from 112 to 192 bytes for the saves and the spill area, which moved where compiled
recursion overflows a 2MiB thread stack from ~2000 levels to ~1800 (measured with the guard lifted);
`MAX_CALL_DEPTH` stays at 500, still with a >3x margin.

Measured on this machine: a tight scalar `while` loop is **~120x** faster compiled (cold/interpreted
vs. warm — `cargo test --release jit_tests::manual_perf_measurement -- --ignored --nocapture`), up
from ~60x when every `LoadL`/`StoreL` went through the buffer — the loop body now touches no memory
at all, and this tier is back ahead of the tracing one on the same loop (whose body still stores its
written locals once per iteration, see Stage 2b). Recursive calls (`fib`) are **~3.5x**
(`jit_tests::manual_recursive_perf_measurement`), unchanged within noise by the register change —
`fib` has a single local, and a call still costs far more than a loop iteration even with the lock
and the allocation gone (marshalling arguments, the depth guard, resolving the callee), just far less
than before.

**Vector indexing inside a compiled loop.** `x[i]` and `x[i]:v` on an `Ints` **parameter or
capture** (not a scratch local — nothing in the compilable subset can construct a fresh vector
value, so only a param/capture is ever actually populated with one at entry) are compilable too,
for a scalar int index. `x[i]` compiles through the same `Op::Call` a plain application (`f x`)
does — this language has no dedicated indexing opcode, a vector applied to an int just *is*
indexing (`src/neant/core/compile.nt`'s `app` node) — so `jitClassifySlots`
(`src/neant/jit/arm64.nt`) walks the bytecode once up front and accepts a param/capture slot only
if *every* appearance of it is one of exactly two shapes: `LoadL(s)` immediately consumed by
`Call(1)` (a read), or the literal 3-op run `TakeL(s); Amend(1); StoreL(s)` `iassign` always emits
back to back (a write). Any other appearance — an ordinary arithmetic read, a bare return of the
vector itself, anything mixing the two — disqualifies that slot and, same as any other unsupported
pattern, just falls back to the interpreter for the whole function; there's no partial/mixed
typing.

Unlike everything above, a vector access doesn't get inlined machine code — it calls out to a
small fixed trampoline (`jit_vec_get`/`jit_vec_set`, `src/jit.rs`), the same shape as `jit_call`.
That's deliberate: a write needs the exact copy-on-write discipline `Op::Amend`/`scatter`
(`src/prims.rs`) already use — `Arc::make_mut`, cloning only if the vector isn't uniquely owned, so
a second live reference never observes the write — and re-implementing that as hand-rolled AArch64
pointer arithmetic against `Arc`'s internal layout would trade a real safety property for a
memory-layout assumption this project has no reason to make. At entry, a classified slot's param/
capture value must be `Ints` (else, same as a non-int plain arg, the whole call just doesn't run
compiled), a clone of it lives in a small side table (`vecbuf`, `try_run`/`try_run_raw`) for the
duration of the call, and that slot's register holds a pointer into it instead of a plain int — the
trampolines bounds-check and deopt exactly like every other guarded point. Measured: **~75x**
(`jit_tests::manual_vector_perf_measurement`, unchanged within noise by the register change) — below the plain scalar loop's ~120x because every
access is still a `blr` (plus the operand-stack spill around it), but the cost this removes
(interpreter dispatch, `Value` boxing per element) still dominates that.


### Stage 2b (started): a tracing JIT for hot loops

The JIT above tiers up whole *functions*, after 64 calls. That misses the shape this language is
most often written in: one call that loops a million times. So a second tier records **traces** —
`src/trace.rs` (the recording side, in the VM's own dispatch loop) and `jitCompileTrace`
(`src/neant/jit/arm64.nt`, the codegen, self-hosted the same way `jitCompile` is).

A loop header is counted every time a backward `Jmp` reaches it (`FnCode::loop_action`,
`src/value.rs`) — per header, not per function, so a loop goes hot inside a single call. At 64, the
VM records the *next* iteration: every op it actually executes, in order, each tagged with the type
it was actually observed to hold. That recording is one straight line with no control flow in it at
all. Where the iteration branched, the trace keeps a **guard** — the direction taken, plus the
bytecode `ip` the other direction would have gone to — and an unconditional `Jmp` leaves no trace
at all, since the ops it skipped simply never ran. So an `if` or a `$[..]` inside the loop costs
nothing until the day its condition actually flips.

Scope for a first pass: `while` only (a `do[n;..]` header is reached with its counter still live on
the operand stack, and recording only starts on an empty one), no calls, no early `Ret`, and
`Push`/`LoadL`/`StoreL`/`Dyad`/`Pop`/`Jmpf`/`Jmp` in the body. Anything else aborts the recording,
and that header is never tried again — the same rejected-once-stays-rejected default the
whole-function JIT already uses. Recording is pure observation: it can't change what a program
computes, only whether some of it gets to run faster.

What a trace buys over the method JIT is types. The whole-function JIT has to *prove* every op is
integer from the bytecode alone; a trace just writes down what the values were, so **floats compile
too** — a second operand stack in `d16..d21` alongside the integer one in `x9..x14`, with the
per-value type tracked at codegen time (`tstack`) rather than a single depth counter. `&` and `|` on
floats are `FMINNM`/`FMAXNM`, not `FMIN`/`FMAX`: Rust's `f64::min`/`max` propagate the non-NaN side,
and the plain forms don't.

**Every local gets a register for the whole loop**, one each from two independent files (x2..x8/x16
for ints, d0..d7 for floats), so the loop body does no memory traffic at all — which is where the
tracing tier pulls ahead of the whole-function one, whose locals stay in the interpreter's own
frame and are loaded and stored on every access. A buffer (`CompiledTrace`, `src/jit.rs`) is only
how they get in and out, indexed by position in a layout `jitCompileTrace` decides and `src/jit.rs`
reads back out of its result rather than recomputing — both halves have to agree about what
position means which local, and one side deciding is what guarantees they do. Entry guards every
one of them (a plain non-null int, or a float, matching what was recorded); anything else, or more
locals than a register file holds, and the loop just runs interpreted.

Every exit is a **bail**: the guard's stub writes the locals back and hands over the `ip` to resume
interpreting at, and `Vm::run_ops` splices them into its own frame and carries on, indistinguishable
from having interpreted the whole time. That's why a guard is only legal where the trace holds no
operand of its own — the interpreter resumes with the operand stack it entered the trace with, the
empty one. The exception is the one deopt that can fire *mid*-iteration: two ordinary ints wrapping
to exactly the null sentinel (`0W+1`), which the interpreter would start propagating as a null from
there on and compiled code would not. Unlike the method JIT's version of that deopt, which just
re-runs the whole call, there are already-committed writes from earlier iterations here. So the loop
stores its written locals to the buffer once at the top of each iteration, and that deopt just
abandons the registers — a half-finished iteration — and resumes at the loop header, where the
buffer still says what the iteration started with. One store per written local per iteration is all
that is left of the body's memory traffic, and the only way to remove even that is to hand the
interpreter the live operand stack at the failing op rather than rewinding to the header.

A `Bool` local is refused outright rather than traced: locals round-trip through a buffer of raw
64-bit words, so `b: i<n` would come back an `Int`, and `type`/`show`/`string` can all see the
difference. On the operand stack a comparison result is fine — nothing there survives the trace.

Measured: **~102x** on a 5M-iteration loop (`jit_tests::manual_trace_perf_measurement`), against a
`do`-loop baseline — the same body written in the one loop form no trace is ever started on, which
is what an interpreted baseline has to be now that a single call to a `while` loop is no longer one.
(That baseline change is also why `manual_perf_measurement` still reports the same ~58x for the
whole-function JIT: what it measures didn't change, only how it gets something interpreted to
measure against. The same loop now runs faster traced than method-compiled, for the register reason
above — the older tier is the one with a buffer in its inner loop.)

What's next here, in rough order: calls, so a trace can cross a function boundary the method JIT
would have to compile whole; `do[n;..]`, which needs the header's live counter handled rather than
avoided; and a deopt that hands back the operand stack instead of rewinding, which would take the
last store out of the loop.

### What the tests check

There is no external oracle left, so the front end is pinned by fixpoints and by behaviour:

- **Generation 2.** Recompile every boot file through the pipeline it defines, then require it to lex,
  parse and compile the corpora to byte-identical output and still run every language case. A compiler
  that does not reproduce its own output when rebuilt by itself fails here — this is what the Rust
  oracle used to catch.
- **The image is a fixpoint.** Compiling the current sources with the embedded image reproduces
  that image exactly. Catches both a stale `image.nb` and a compiler change rebuilt only once.
- **The language cases.** ~250 source/result pairs — semantics, error messages, error line numbers and
  call stacks — every one through `nrun`.
- **Front-end errors.** Lexer and parser messages and their line numbers, asserted literally.
- **The JIT against the interpreter.** Every compiled path is checked by running the same loop twice
  — once so it tiers up, once in a form that provably never can (a spliced-in `- -`, an identity the
  compilability check rejects; or a `do` loop, which no trace is started on) — and requiring the two
  to agree. That covers the guards and deopts too: a branch that flips after the trace was recorded,
  an `0W+1` wrapping to the null sentinel mid-loop, an out-of-range index, a second live reference to
  an amended vector. One test asserts a wall-clock bound instead, since everything else here would
  still pass if the JIT silently stopped compiling anything at all.
- RFC vectors for the crypto and the TLS key schedule; the record layer round-trips offline.

What this gives up relative to the oracle: a bug that the compiler introduces *and* reproduces
consistently is no longer caught by construction — it is caught only if a language case exercises it.

### Performance

- `x,: y` compiles to Take+join so appends are in place — 20k appends, 444ms → 1ms.
- Globals are interned to slots at load, so `LoadG` is an index, not a hash. Execution stacks are pooled.
- `?` `distinct` `group` hash atoms — 200k ints over 1000 keys: distinct 159ms → 6ms, group 198ms → 10ms.
  Nested keys fall back to a scan.
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

Together: the self-hosted rebuild of the sources takes ~185ms (was ~324ms), and a `while` iteration
costs 26ns. On the crypto in `src/neant/crypto/crypto.nt`, per 64KB: SHA-256 284ms → 119ms, ChaCha20 115 → 68,
Poly1305 61 → 34, the AEAD 198 → 103; X25519 76ms → 42, and a TLS 1.3 handshake against OpenSSL
169ms → 97ms. Allocation went from ~34% of samples to under 1%; what is left is the dispatch loop
itself and `Value` clone/drop.

Runtime errors carry a line table, which costs ~10% of compile throughput. A frame is named by its
caller's `LoadG`, so the bytecode carries positions but no names.

### Next

For the JIT, the next steps are in "Stage 2b" above: locals in registers across a traced iteration,
then calls, then `do[n;..]`.

A register-style calling convention was tried and reverted — it measured slower, and the profile
said frame setup is ~5% while `Value` clone/drop and small-list allocation are ~35%. The allocation
half of that is now gone (see above). What the profile shows next is `execute` itself and `Value`
clone/drop — Rc traffic through the operand stack — so the remaining levers are moving VM dispatch
into neant-generated specialised code, and a user-function call, which still costs ~37ns of frame
setup on top of its body.

For TLS, what is missing is ASN.1 DER, RSA/ECDSA signature verification, and a root store.

`tlsConnect` used to draw the x25519 private key from `rand`, and then the ClientHello random from it
too. That random goes out in the clear, xorshift64 is linear and invertible, and 32 bytes of it are
enough to solve for the state and roll back to the key — a passive break, no certificate needed. Key
material now comes from `urand`, and `rand` keeps the reproducibility its tests want.

Ed25519 is in (`src/neant/crypto/ed25519.nt`), which took one runtime primitive — `badd` — and no language
change. The remaining signature work is RSA-PSS and ECDSA P-256, which real certificates actually use;
both need a general modular reduction (Montgomery or Barrett) rather than the special-prime folding
the 2^255-19 field gets away with. RSA *verification* stays cheap because the exponent is 65537.
