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
| Statistics | `wsum wavg cov cor svar sdev zscore quantile percentile mode` |
| Math | `sqrt floor ceiling round signum neg mod div xexp` |
| Lists | `til first last reverse raze sort asc desc distinct where rank rotate cut sublist except inter union cross` |
| Collections | `sortOn maxBy minBy freq groupBy takeWhile dropWhile partition zip flatten` |
| Running, windowed | `sums prds maxs mins deltas ratios prev next differ msum mavg mmax mmin ema xbar bin` |
| Iteration | `prior converge converges iterate iterates` |
| Strings | `string sym vs sv ss upper lower trim ltrim rtrim ssr like hex unhex` — regexes: [below](#regular-expressions) |
| Formatting | `tostr lpad rpad lpad0 fixed commas fmt` |
| Dates, times | `today dow ymd isleap dim mkdate addMonths mstart mend ystart wstart iso isot isodt pdate ptime pdt dfmt dparse httpDate` |
| Tests | `type not in within` |

```
prior[-;1 5 20]                    // 1 4 15      f over adjacent pairs, first kept (deltas is prior[-])
converge[{_x%2};100]               // 0           apply until the value stops changing; converges keeps the steps
iterate[3;{x*2};1]                 // 8           n times; iterates gives 1 2 4 8
2 vs 13   256 vs 1000   0x00 vs 258  // 1 1 0 1   3 232   0x0000000000000102   base decomposition, msd first
24 60 60 sv 1 2 3   2 sv 101b      // 3723   5     and back (an int left argument picks the numeric vs/sv)
fmt["%s has %d items (%5.1f%%)"; ("bob";3;42.25)]   // "bob has 3 items ( 42.3%)"   %s %d %f, - width .prec, %% ; bare % is %s
fixed[2;3.14159]   commas 1234567   lpad[6;42]      // "3.14"   "1,234,567"   "    42"
quantile[0.5;3 1 2]   cor[1 2 3;2 4 6]   mode 1 2 2  // 2f   1f   2       R type-7 interpolation; svar/sdev are the n-1 forms
sortOn[count;("aa";"b")]   freq "abca"   groupBy[{x mod 2};til 5]   // ("b";"aa")   "abc"!2 1 1   0 1!(0 2 4;1 3)
dow 2026.09.16   mkdate[2026;9;16]   addMonths[1;2026.01.31]    // 2 (Mon=0)   2026.09.16   2026.02.28 (clamped)
iso d   isodt[d;t]   pdt "2026-09-16T12:30:00.250Z"          // "2026-09-16"   "...T12:30:00.250Z"   (date;time)
dfmt["%a, %d %b %Y %H:%M:%S GMT"; d; t]   dparse["%Y/%m/%d";"2026/9/6"]   // httpDate[d;t] is that format; codes Y m d H M S b a j y
```

### Rust builtins

Only what needs the host; everything expressible with the verbs lives in the prelude instead:

| Group | Builtins |
|---|---|
| Math | `exp log sin cos tan atan` |
| Random | `rand rseed` — `n rand m` draws n from `[0;m)` or from the list m, `rseed 7` makes a run reproducible. `urand n` is n bytes from the OS: `rand` is a PRNG seeded from the clock, so keys come from `urand` |
| Bits | `badd band bor bxor shl shr bnot` — on the raw 64-bit pattern; `badd` is `+` without the int-null case, for u64 words |
| Dicts | `key value group` |
| Values | `isnull now` |
| Output | `show print signal exit repr` — `repr x` is the text `show` would print, the one thing `$` cannot give (`$` casts elementwise); `tests/lang.nt` pins display forms with it |
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
| Schema | `meta tcols xcol xcols` |
| Query | `tsel tsort tby fby xasc xdesc ungroup` |
| Update | `tupd tdel tdistinct tinsert` |
| Joins | `lj ij uj aj` |
| Keyed | `xkey unkey` |
| CSV | `tcsv wcsv rcsv` |

```
select total: sum pay, n: count pay by dept from t where pay>90
                             // q-style select, desugared by the parser into qsel[t;where;by;cols]
t[1]  t[0 2]                 // rows by position; t[where t[`pay]>90]
kt: xkey[`id;t]; kt 3        // keyed table: key rows -> remaining columns; kt[(1;2)] for a multi-column key
(1 2;3 4)?3 4                // ? on a general list finds a whole row (1); so does `in`
meta t                       // `c`t table: column name and type (`syms `ints ... `list for a general-list column)
tcols t                      // `name`dept`pay
xcol[`pay`dept!`p`d; t]      // rename by dict old!new;  xcol[`a`b; t] renames the first two columns
xcols[`pay; t]               // those columns first, the rest keep their order
tcsv t                       // "name,dept,pay\nann,eng,120\n..." — a cell holding , " or a newline is "" quoted
wcsv["t.csv"; t]             // write0 that text;  rcsv["SSI";",";"t.csv"] reads it back — or pass the CSV text itself
fby[avg;`pay;`dept;t]        // per-row: avg pay of the row's dept, aligned with t;  tsel[t; t[`pay]>fby[avg;`pay;`dept;t]]
tupd[t; t[`dept]=`ops; (,`pay)!enlist {x[`pay]+5}]   // update masked rows: name -> vector, atom, or unary fn of the masked rows
tupd[t; (); (,`n)!enlist 0]  // () means every row; a new name adds a column, other rows filled with nullof
tdel[t; t[`pay]<90]          // the rows where the mask is 0b;  tdel[t;`note] or tdel[t;`a`b] drops columns
tdistinct t                  // distinct rows, first occurrence kept
tinsert[t; (`dan;`ops;90)]   // append one row: a list in column order, or a dict `name`dept`pay!(...)
```

## JSON

`src/neant/stdlib/json.nt`: `jk` parses (objects are dicts, null is `::`), `jj` serializes.

```
jk "{\"a\": [1, 2]}"         // ,`a!(1 2)
jj `a`b!(1 2;"x")            // "{"a": [1, 2], "b": "x"}"
```

## Regular expressions

`src/neant/stdlib/regex.nt`: a backtracking engine in neant — pattern first, a match is `(start;length)`.
Literals `. \d \w \s` (and their negations) `[a-z] [^...] ^ $ ( ) (?: ) |` and `* + ? {n} {n,m}` with the lazy
`*? +? ??` forms; case-sensitive, no backreferences or lookaround.

```
rxTest["\\d+";"ab12"]                  // 1b           rx["\\d+";"ab12"] -> 2 2 (start;length), () if none
rxAll["a";"banana"]                     // (1 1;3 1;5 1)
rxCaps["(\\w+)@(\\w+)";"to bob@ex"]     // ("bob@ex";"bob";"ex")   whole match, then the groups
rxSub["(\\w+)@(\\w+)";"$2:$1";"bob@ex"] // "ex:bob"     $0..$9 are groups, $$ a literal $
rxSplit[", *";"a, b,c"]                 // ("a";"b";"c")
```

The pattern is compiled once to a small program run by a machine with an explicit backtrack stack, and a
quantifier over one character (`[a-z]*`) counts the run with a vector op and tries the lengths from there — so
neither a long subject nor a long run recurses, and `(a*)*` terminates. A malformed pattern signals `regex: ...`.

## Tests in neant

`src/neant/stdlib/test.nt` is a small harness — `tok[name;cond]` `teq[name;got;want]` `terr[name;f]`
`terrLike[name;f;prefix]` `tsection` `treport[]` — and `tests/*.nt` are the stdlib tests written with it:

```
./target/release/neant tests/run.nt      # loads every tests/*.nt by name, prints "N passed, M failed", exits with M
```

`tests/lang.nt` is the language itself — the 363 source/result pairs that used to be the `CASES` table in
src/main.rs. `cargo test` runs the whole set twice: once directly (`nt_tests`) and once against the
front end rebuilt by itself (`front_end_reproduces_itself`), both in src/main.rs. So a language or stdlib
change is tested where it lives: add a `teq` line to the matching `tests/*.nt` rather than a case in Rust.

## Encodings

`src/neant/stdlib/encode.nt`: base64 and percent-encoding on the byte vectors `` `byte$ `` gives, and query strings.

```
b64 `byte$"foobar"           // "Zm9vYmFy"    unb64 "Zm9vYmFy" -> 0x666f6f626172;  b64url/unb64url are the - _ unpadded form JWTs use
urlenc "a b/é"               // "a%20b%2F%C3%A9"   RFC 3986: unreserved chars pass, every other UTF-8 byte is %XX
urldec "a%20b+c"             // "a b c"       + reads as a space, like a form
qparse "a=1&b=x+y"           // `a`b!("1";"x y")     qbuild inverts it
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

`req` is `` `method`target`path`query`version`headers`body!(...) `` — `path` percent-decoded with the
query string split off into `query`, a dict of strings keyed by symbol (`target` is the raw request-target);
headers keyed by lowercased symbol (build
one with `` `$"content-length" ``, not a literal `` `content-length `` — a hyphen in a *literal*
symbol token is the `-` verb, not part of the name; casting a string with `` `$ `` has no such
limit). A handler returns `(status; reason; headers; body)`. No chunked transfer-encoding, no
keep-alive (`hclose` after every response), no HTTPS yet — `src/neant/crypto/tls.nt` is
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
(lex/parse/compile, above) and `stdlib/` (prelude/table/json/encode/regex/test) do; `crypto/` is split between what's
in it (`crypto.nt`) and what's loaded on demand (`ed25519.nt`, `tls.nt` — see their own sections);
`net/` (`http.nt`) is loadable only. `load "f.nt"` doesn't care which directory a file is under.

### Stage 2 (started): a baseline JIT for integer loops and calls

The codegen is self-hosted the same way Stage 1 is: `src/neant/jit/arm64.nt` is neant, and its entry
points — `jitCompile` here, `jitCompileTrace` for [Stage 2b](#stage-2b-started-a-tracing-jit-for-hot-loops)
— take bytecode as data and return either a `Bytes` value (the encoded AArch64) or `::` for "not
compilable", the null-signals-failure convention used elsewhere. Only what genuinely needs the host
stays in Rust (`src/jit.rs`, AArch64 only, no crate dependency): `mmap`/`mprotect`/`__clear_cache`
declared directly as `extern "C"` (already linked in via libc/libgcc), ownership of the executable
memory, and the trampolines compiled code reaches through `blr`.

Both tiers are fail-closed the same way, and the rest of this section and Stage 2b assume it:
whatever fails the check is rejected once and runs interpreted forever after, and compiled code is
guarded at entry and can still bail out mid-run (`deopt`) back to the interpreter, which is always
correct and always present. That is safe because the compilable subset cannot observe anything
outside its own locals, so re-running a compiled stretch from scratch — or abandoning one halfway —
is never visible.

One wrinkle is unique to a JIT that compiles itself: the codegen's own bytecode contains the very op
kinds it exists to handle, so once one of its functions goes hot, compiling it would mean *calling*
it to walk its own bytecode, reentering its own `OnceLock` from inside that lock's initializer. A
thread-local guard (`already_compiling`) shuts compilation off for the whole nested call — the JIT's
own implementation is never a JIT target, which only affects how long one-time compilation takes.
(Compiling at all means running neant code, so it needs `&mut Vm` even from inside a trampoline
holding a raw `*mut Vm`; that reborrow is sound under the discipline every reentrant
interpreter-calls-host-calls-interpreter path already relies on, adverbs included — single-threaded,
strictly nested, the outer call's own `&mut self` untouched while the nested one runs.)

**This tier compiles a whole `FnCode`, after 64 calls**, if every op in it is provably pure integer
arithmetic over its own locals — `+ - * & | < > =`, `Push`/`LoadL`/`StoreL`/`Pop`/`Ret`,
`Jmp`/`Jmpf`/`Loop` (`while`/`if`/`do` control flow), and a call to another function that is
*itself* provably pure the same way. Anything else (a global that isn't an immediately-called
function, a closure, a float, a call to something impure) is rejected. The result is cached behind a
`OnceLock` (`FnCode::jitted`, `src/value.rs`) — built complete, then stored once, never edited in
place, so two threads racing to compile the same hot function waste one's work instead of racing on
it: the atomic-swap-in discipline [Concurrency](#concurrency) already commits to.

`Loop` (`do[n;..]`) is the one op whose two edges leave the interpreter's stack at different depths
— decrementing in place and falling through leaves it unchanged, but exiting also pops — so it is
the one place the compiler cannot just accumulate stack depth linearly through the bytecode; the
exit edge's depth is recorded and overrides that accumulation when the scan reaches it, which is
also what makes nested `do` loops compile.

Entry demands a plain non-null int for every argument and capture. After that, the deopt points are:
two ordinary ints wrapping to exactly the null sentinel (`0W+1`), which the interpreter would
propagate as a null and compiled code would not; a call whose arity doesn't match or whose callee
turns out not to be a plain function; and runaway recursion, since compiled-to-compiled calls go
through `blr` rather than `Vm::call_code`'s own depth guard. That last one has a limit of its own
(`MAX_CALL_DEPTH`) *and* counts against the interpreter's (`vm::MAX_DEPTH`), because a compiled
chain is entered from some interpreted depth and sits on top of it — both calibrated against the
smallest stack this can run on, a `spawn`ed thread's 2MiB, not the main thread's.

Calling another compiled function goes through one fixed trampoline (`jit_call`): it resolves the
callee exactly like `Op::LoadG` would, proves it pure *before* making the call at all — the only way
a later deopt of the caller cannot fire a real side effect twice — then recurses through compiled
code directly, never dropping back into the interpreter unless something deopts. The callee's
compiled version is cached behind its own `OnceLock` (`jit_for_call`), read with a plain atomic load
on every recursive step, and its locals go on a fixed-size native stack buffer (`try_run_raw`)
rather than a heap `Vec` — so a hot recursive call pays neither a lock nor an allocation.

**Locals live in registers.** That buffer is only how a call's locals get *in*: every local a
compiled function touches — params, scratch and captures alike, in first-seen bytecode order
(`jitLocalSlots`) — gets one register for the whole function, loaded once in the prologue, and
`LoadL`/`StoreL` become register moves that are never written back. Sound because `Compiled::run`
reads only the returned value and the ok word, and neither entry point looks at the buffer again, so
a return or a deopt simply abandons the registers. Unlike the tracing tier's version of the same
idea, a compiled function makes calls and its locals have to survive them: the first six registers
handed out (`jitLREGS`) are callee-saved (x23..x28), preserved by the trampolines for free at one
`stp`/`ldp` per pair actually used; the next five are caller-saved (x6..x8, x16, x17) and spilled
around every `blr` the way the live operand stack already was; a twelfth local and beyond stays in
the buffer, reached through x19 exactly as every local used to be, so no function is rejected for
being too wide. A vector-classified slot (below) is a register too — its value is a pointer, a plain
64-bit word only ever handed to the vector trampolines. The frame grew from 112 to 192 bytes, which
moved where compiled recursion overflows a 2MiB stack from ~2000 levels to ~1800.

**Vector indexing inside a compiled loop.** `x[i]` and `x[i]:v` compile for a scalar int index on an
`Ints` **parameter or capture** — not a scratch local, since nothing in the compilable subset can
construct a vector. This language has no indexing opcode: a vector applied to an int just *is*
indexing (`compile.nt`'s `app` node), through the same `Op::Call` a plain application emits. So
`jitClassifySlots` walks the bytecode once and accepts a slot only if *every* appearance of it is
one of exactly two shapes — `LoadL(s)` immediately consumed by `Call(1)` (a read), or the literal
3-op run `TakeL(s); Amend(1); StoreL(s)` that `iassign` always emits back to back (a write). Any
other appearance disqualifies it; there is no partial typing.

The access itself is the one thing not inlined as machine code but called through a trampoline
(`jit_vec_get`/`jit_vec_set`), deliberately: a write needs the exact copy-on-write discipline
`Op::Amend`/`scatter` (`src/prims.rs`) already use — `Arc::make_mut`, cloning only if the vector
isn't uniquely owned, so a second live reference never observes the write — and hand-rolling that as
pointer arithmetic against `Arc`'s internal layout would trade a real safety property for a
memory-layout assumption this project has no reason to make. At entry the slot's value must be
`Ints`; a clone lives in a side table (`vecbuf`) for the call's duration and the slot's register
holds a pointer into it. The trampolines bounds-check and deopt like every other guarded point.
Numbers for both tiers are together at the end of Stage 2b.

### Stage 2b (started): a tracing JIT for hot loops

The tier above tiers up whole *functions*, after 64 calls. That misses the shape this language is
most often written in: one call that loops a million times. So a second tier records **traces** —
`src/trace.rs` does the recording, inside the VM's own dispatch loop, and `jitCompileTrace` the
codegen.

A loop header is counted every time a backward `Jmp` reaches it (`FnCode::loop_action`) — per
header, not per function, so a loop goes hot inside a single call. At 64 the VM records the *next*
iteration: every op it actually executes, in order, each tagged with the type it was actually
observed to hold. What comes out is one straight line with no control flow in it at all. Where the
iteration branched, the trace keeps a **guard** — the direction taken, plus the bytecode `ip` the
other direction would have gone to — and an unconditional `Jmp` leaves nothing behind, since the ops
it skipped never ran. So an `if` or a `$[..]` inside the loop costs nothing until the day its
condition actually flips. Recording is pure observation: it cannot change what a program computes,
only whether some of it gets to run faster. Scope is `while` and `do[n;..]` loops,
`Push`/`LoadL`/`StoreL`/`Dyad`/`Pop`/`Jmpf`/`Jmp`/`Loop` in the body, and calls to plain lambdas
held by globals (both below).

What a trace buys over the method tier is types. That tier has to *prove* every op integer from the
bytecode alone; a trace just writes down what the values were, so **floats compile too** — a second
operand stack in `d16..d21` beside the integer one in `x9..x14`, with a per-value type tracked at
codegen time (`tstack`) rather than a single depth counter. `&` and `|` on floats are
`FMINNM`/`FMAXNM`, not `FMIN`/`FMAX`: Rust's `f64::min`/`max` propagate the non-NaN side and the
plain forms don't. Locals get a register each here too (`jitTrLOCALS`: x2..x8/x16 for ints, d0..d7
for floats), and the buffer they pass through (`CompiledTrace`, `src/jit.rs`) is indexed by a layout
`jitCompileTrace` decides and `src/jit.rs` reads back out of its result rather than recomputing —
both halves have to agree what a position means, and one side deciding is what guarantees they do.

**Every exit hands the interpreter its operand stack.** A guard's stub writes the locals back, then
whatever the trace's own virtual stack holds at that point into the same buffer past the locals, and
returns an *index* into an exits table `jitCompileTrace` returns alongside the code — one
`(resume ip; stack tags)` per exit. `CompiledTrace::run` rebuilds those values by tag, `Vm::run_ops`
pushes them and resumes at that `ip`, indistinguishable from having interpreted the whole time. So a
guard is legal anywhere, not only where the stack happens to be empty: a `Jmpf` inside an expression
hands back that expression's operands, and the one deopt that fires *mid*-iteration — `0W+1`
wrapping to the null sentinel — resumes at the op *after* the colliding one with the null result on
top, which is exactly the `Int` the interpreter would have produced and propagates from there. A
trace with no rewind exit (below) therefore has **no memory operation in its body at all**.

Tags are why `Bool` is its own type on the virtual stack: a comparison result handed back has to
come back a `Value::Bool` — `type` sees the difference, and so does `&`/`|`, whose result is a bool
exactly when both operands are (`1b&0b`, not `2&1b`), a rule codegen reproduces and the recorder's
observed result type double-checks. In registers a bool is a 0/1 like any int. A `Bool` *local* is
still refused: locals round-trip through the buffer as raw words of their slot's one type, so
`b: i<n` would come back an `Int`.

**`do[n;..]` loops.** A `do` header is the `Op::Loop` that tests and decrements its counter, and it
is reached with that counter live on the operand stack — which is what kept `do` from tracing. Now
the entry stack is part of the trace: recording starts (and a compiled trace is entered) when the
stack holds only plain non-null ints (`Trace::entry`), and those are loop-carried values — on the
virtual stack from step 0, back in the same registers at the back edge, handed back at every exit
like anything else on it. `Op::Loop` is a step of its own: on the edge the recording took, a guard
that bails to the loop's exit with the counter popped if it is ever `<= 0` — which *is* the
interpreter's other edge, stack and all — plus a decrement in place. A `do` nested inside records
unrolled, its exit edge a guard in the other direction bailing to the `Loop` op itself; a `while`
inside a `do` traces with the outer counter under it the whole time; `do` inside `do` traces with
two counters on its entry stack. An `Op::Loop` inside an inlined callee is rejected rather than
rewound — its bail would be into the callee's bytecode and, unlike a callee's branch, it mutates the
stack — and a counter that isn't a plain int (`do[1b;..]` and `do[2.0;..]` are legal, `int_of` takes
both) is refused at the entry check or, nested, by its tag at codegen.

**Calls are inlined, not called.** The recorder lives on the `Vm` rather than in one `run_ops`
frame, so when the loop body calls a plain lambda it follows the interpreter into the callee's frame
(`Recorder::enter_frame`/`exit_frame`) and keeps writing down what runs. What comes out is still one
flat sequence with no call in it: a `FramePush` remembering how deep the operand stack stood under
the arguments (that is where the result has to end up), a `StoreL`+`Pop` per argument binding it
into a slot of the callee's own, the body's ops exactly as in the loop, and a `FrameEnd` moving the
one value the frame leaves behind to where the caller expects it. Each inlined frame gets a disjoint
range of trace-local slots past the loop's own (`real_upto`), and those are **virtual**: a register
like any other local, but never loaded, stored or written back — they have no value before the loop
and nobody wants one after. So `f[a;b]`, `f[g[x]]`, a callee that calls another lambda, the same
lambda at two call sites, a callee with a scratch local or an early `:x` all inline, and a bounded
recursion inlines as far as it actually recursed. The method tier is bypassed for a callee while a
recording is in progress — the recorder has to *see* the callee's ops — which only defers a
tier-up, the recording being one iteration long.

Two kinds of guard fall out of that. **Which function the global holds** is checked once per entry —
enough, since nothing a compiled trace runs can assign a global — and a trace whose callee was
reassigned is retired rather than refused forever: the header counts afresh and is recorded again
against the new definition, up to `MAX_RETRACE` (4) times (`FnCode::retrace`). Reassigning a
function at the REPL between runs is ordinary; a loop whose callee changes on every run is not worth
a compile each time. **A branch inside a callee** cannot bail the way one in the loop's own frame
does, because the `ip` it would resume at is in a frame the interpreter is not in. Those are
`GuardRewind`s, the one exit left that does not hand off: it throws the half-finished iteration away
and resumes at the loop header from the values the buffer says the iteration started with — which is
why a trace that contains one stores its written locals and entry stack there at the top of every
iteration. An int-null collision inside a callee rewinds the same way. If a callee's condition flips
for good, every later iteration enters the trace, rewinds and is interpreted; measured, that costs
nothing over interpreting alone (496ms against 510ms for 2M such iterations), so there is no cliff
to fall off. Everything that is *not* a plain lambda in a global — a closure, a primitive, a
projection (an arity mismatch is one), a vector being indexed, a callee that reads a global, a `Ret`
out of the loop's own frame — fails the recording and leaves the loop interpreted, never
miscompiled: a `Call` that didn't become an inlined frame arrives at the recorder without one having
`returned`, and that is the whole check.

**Measured** on this machine, `cargo test --release -- --ignored --nocapture`. Every ratio is
against the same body with `- -` (two monadic negations, an identity) spliced in: `Op::Monad` is
rejected by both tiers, so the twin is guaranteed interpreted. It does marginally more work per
iteration than the original, so each number is a few percent optimistic.

- **~190x** — a scalar `while` loop on the method tier (`manual_perf_measurement`): 0.95ms against
  185ms, 1M iterations.
- **~160x** — the same shape on the tracing tier (`manual_trace_perf_measurement`): 5.5ms against
  890ms, 5M iterations. No branch and no call in the body, so the trace has no rewind exit and
  therefore no memory operation at all; that removed store was worth ~5%.
- **~195x** — a loop calling `{x*2}` every iteration (`manual_trace_call_perf_measurement`): 5.8ms
  against 1.14s, 5M iterations, the twin paying a real `Op::Call` → `call_code` → `execute` each
  time. The same speed as the loop with no call in it, which is what inlining should mean.
- **~112x** — a `do` loop (`manual_trace_do_perf_measurement`): 6.4ms against 725ms, 5M iterations.
  One compare-branch-decrement per iteration more than the `while` form, which shows.
- **~73x** — `x[i]` inside a compiled loop (`manual_vector_perf_measurement`): below the scalar
  loop because every access is still a `blr` plus the operand-stack spill around it, but the cost
  it removes (interpreter dispatch, `Value` boxing per element) still dominates that.
- **~3.5x** — recursive calls, `fib 27` (`manual_recursive_perf_measurement`). A call costs far more
  than a loop iteration even with the lock and the allocation gone: marshalling arguments, the depth
  guard, resolving the callee.

### What the tests check

There is no external oracle left, so the front end is pinned by fixpoints and by behaviour:

- **Generation 2.** Recompile every boot file through the pipeline it defines, then require it to lex,
  parse and compile the corpora to byte-identical output and still pass every `tests/*.nt`. A compiler
  that does not reproduce its own output when rebuilt by itself fails here — this is what the Rust
  oracle used to catch.
- **The image is a fixpoint.** Compiling the current sources with the embedded image reproduces
  that image exactly. Catches both a stale `image.nb` and a compiler change rebuilt only once.
- **The language cases.** 363 source/result pairs — semantics, error messages, error line numbers and
  call stacks — every one through `nrun`. They live in `tests/lang.nt`, in neant: a case is
  `("1+2"; "3")`, run with `repr join spawn {nrun s}`, where the `spawn` gives it the fresh globals
  the Rust harness used to get from `snapshot`/`restore`.
- **Front-end errors.** Lexer and parser messages and their line numbers, asserted literally.
- **The JIT against the interpreter.** Every compiled path is checked by running the same loop
  twice — once so it tiers up, once with `- -` spliced in so it provably never can — and requiring
  the two to agree, at sizes that straddle the tier-up threshold. The guards and deopts get the same
  treatment: a branch that flips after the trace was recorded, `0W+1` wrapping to the null sentinel
  mid-loop under an operand stack of every shape, a bool that has to come back a bool, an
  out-of-range index, a second live reference to an amended vector, a callee's global reassigned
  between runs. So does every way a shape can *refuse* to compile — a `do` inside a callee, a
  non-int counter, a closure or primitive or projection callee, an arity mismatch, a global read,
  deep recursion, too many locals — since what matters there is that it be rejected rather than
  miscompiled. One test asserts a wall-clock bound instead, since everything else here would still
  pass if the JIT silently stopped compiling anything at all.
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

For the JIT: int/float promotion inside a trace, so `1.0*i` compiles; side traces for a branch that
flips for good, which currently bails on every iteration and runs the rest of it interpreted (no
cliff, but no gain either); and a rewind-free exit for a branch inside an inlined callee, which
needs the interpreter to be able to resume inside a frame it never entered.

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
