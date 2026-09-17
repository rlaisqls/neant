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
| Files | `read0 write0` — plus `read1 n` / `write1 x`, `hrecv`/`hsend` for stdin/stdout: exactly n bytes in, exactly these bytes out, for a byte-counted protocol on the standard streams (`src/neant/tools/lsp.nt`) |
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
stall every other socket in the process, exactly the concurrency this is for. No TLS server side —
`src/neant/crypto/tls.nt` is a client only, though its handshake does authenticate the server it
talks to ("Bytes and crypto" below).

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
src/main.rs — and `tests/jit.nt` and `tests/crypto.nt` are the JIT and crypto suites that used to be Rust
`#[test]` functions. `tests/bench.nt` is not loaded by `run.nt`: it measures the tiers rather than
asserting on them, so it is run by hand (`./target/release/neant tests/bench.nt`). `cargo test` runs the whole set twice: once directly (`nt_tests`) and once against the
front end rebuilt by itself (`front_end_reproduces_itself`), both in src/main.rs. So a language or stdlib
change is tested where it lives: add a `teq` line to the matching `tests/*.nt` rather than a case in Rust.

## Tooling

A formatter and a language server, both in neant, both loadable rather than in the boot image. They
do not reimplement anything: the front end is already a library — `nlex`, `nparse` and `ncompile`
are ordinary globals — so the formatter tokenises with the lexer's own dispatch and the server's
diagnostics are the compiler's own errors.

### The formatter

`src/neant/tools/fmt.nt`: `nfmt[src]` returns the formatted text, `nfmtFile[path]` rewrites a file
and returns `1b` if it changed.

```
load "src/neant/tools/fmt.nt"
nfmt "f: {[a]\nb:   a+1  \nb}"        // "f: {[a]\n  b:   a+1\n  b}"
nfmtFile "src/neant/stdlib/json.nt"   // 0b — already formatted
```

Whitespace here is load-bearing — `1 -2` is a vector and `1 - 2` is subtraction, `f ,x` parses as
`f , x`, a newline ends a statement, a name followed by a verb is that verb's left argument — so a
formatter that reflows breaks code. This one never moves a token across a line, never adds or
removes a line, and touches only five things: trailing whitespace; indentation, two spaces per
open bracket, skipping the lines of a `;`-broken argument list, which the author aligned by hand;
whitespace after `( [ {`; whitespace before `) ] } ;`; and a run of spaces after `;` collapsed to
one. Everything else is copied byte for byte, aligned definitions and aligned trailing comments
included. `src/neant/tools/fmt.nt`'s header comment is the exact list.

Meaning-preservation is mechanical rather than argued. `nparse` reads nothing but the token list
and the line each token starts on, so identical tokens and identical lines are an identical AST —
and every rewritten line is re-lexed and reverted to the original unless `nlex` gives back exactly
what it gave before. `tests/fmt.nt` runs that check over **every `.nt` file in the repository**,
compares the compiled bytecode as well, and requires `nfmt nfmt s` to equal `nfmt s`. Over the
whole repository it changes 3 lines, which is the house style being what the rules say.

### The language server

`src/neant/tools/lsp.nt`: running the file starts an LSP server.

```
./target/release/neant src/neant/tools/lsp.nt                  # over stdin/stdout
./target/release/neant src/neant/tools/lsp.nt 127.0.0.1:5007   # over TCP — what an editor wants
```

`initialize`, `initialized`, `shutdown`, `exit`, `textDocument/`{`didOpen`, `didChange` (full sync),
`didClose`, `publishDiagnostics`, `formatting`, `documentSymbol`, `hover`, `completion`}, and the
capabilities it advertises are exactly those. Diagnostics are the point: the buffer goes through
`nlex` then `nparse` inside `@[..]` and a signalled error becomes one diagnostic on the line its
message names, so what an editor underlines is what the compiler would have said. Formatting is
`nfmt`. Symbols are the top-level `name:` assignments. Hover and completion cover those plus the
names in the running image — there is no way to enumerate the globals, so the candidates come from
the boot sources plus a written-down list of the Rust builtins, and each one is *confirmed against
the image* with a `loadg` of one const before it is offered.

Both transports run the same loop over different byte sources: `lspServe[]` on stdin/stdout, which
is what an editor launches by default, and `lspServeTcp[addr]` on a socket. Framing needs reads and
writes that are byte-exact and incremental — `read0 0` reads stdin to EOF and splits lines, `print`
appends a newline, `write0 "/dev/stdout"` truncates on every call — so stdio needed two primitives
that genuinely have to be in the host: `read1 n` and `write1 x`, `hrecv`/`hsend` for the standard
streams. editors/README.md has the configuration for neovim, eglot and VS Code, and covers the
tree-sitter grammar and vim syntax file next to it.

## Encodings

`src/neant/stdlib/encode.nt`: base64 and percent-encoding on the byte vectors `` `byte$ `` gives, and query strings.

```
b64 `byte$"foobar"           // "Zm9vYmFy"    unb64 "Zm9vYmFy" -> 0x666f6f626172;  b64url/unb64url are the - _ unpadded form JWTs use
urlenc "a b/é"               // "a%20b%2F%C3%A9"   RFC 3986: unreserved chars pass, every other UTF-8 byte is %XX
urldec "a%20b+c"             // "a b c"       + reads as a space, like a form
qparse "a=1&b=x+y"           // `a`b!("1";"x y")     qbuild inverts it
```

## HTTP

`src/neant/net/http.nt` (loadable, not in the boot image) is a client and a server. A connection is
a triple of functions — `(read; write; close)`, where `read[]` gives the next bytes and empty means
end of stream — so `httpPlain h` puts it on an `hopen`/`accept` handle and `httpTls h` on a
`tlsConnect` one, and everything else is written against the triple. That is the whole of what
HTTPS needed: `tlsSend`/`tlsRecv` ("Bytes and crypto") already have the shape `hsend`/`hrecv` have.

```
load "src/neant/crypto/tlsclient.nt"       // only for https:// — it is what verifies the chain
load "src/neant/net/http.nt"
r: httpGet "https://example.com/"
r`status                                    // 200
`char$r`body                                // the body; bodies are BYTES, not text
```

`httpGet` follows up to five redirects; `httpFetch[url;method;headers;body]` does the one exchange
and hands the `Location` back instead. `httpPost[url;headers;body]` posts. Bodies are bytes in both
directions, because a response may be gzip or an image and decoding that as UTF-8 corrupts it.

A connection is reusable, which is what makes a verifying TLS client usable at all — the handshake
is the expensive part and nothing about it needs repeating:

```
c: httpOpen "https://example.com/"; u: urlParse "https://example.com/"
httpExchange[c; u; "GET"; "/a"; ()!(); ""]      // handshake ~1.4s, this exchange ~60ms
httpExchange[c; u; "GET"; "/b"; ()!(); ""]      // ~60ms
(c 2)[]                                          // close
```

Those two numbers are the shape of the thing rather than a fixed cost: ~95% of a first request is
the handshake, and nearly all of that is signature verification (`p256.nt`/`p384.nt`), which is
being worked on — so the handshake figure is expected to move by an order of magnitude and the
~60ms, which is a network round trip, is not.

The server is `httpRecv`/`httpSend` plus `httpServe`, which wraps the accept-loop-plus-`spawn`
pattern shown earlier (`hlisten`/`accept`, "Rust builtins" above) into one call:

```
l: hlisten "0.0.0.0:8080"
httpServe[l; {[req] (200; "OK"; (`$"content-type")!(,"text/plain"); "you asked for ",req[`path])}]
```

`req` is `` `method`target`path`query`version`headers`body!(...) `` — `path` percent-decoded with the
query string split off into `query`, a dict of strings keyed by symbol (`target` is the raw request-target);
headers keyed by lowercased symbol (build
one with `` `$"content-length" ``, not a literal `` `content-length `` — a hyphen in a *literal*
symbol token is the `-` verb, not part of the name; casting a string with `` `$ `` has no such
limit). A handler returns `(status; reason; headers; body)`.

Chunked transfer-encoding is decoded on both sides, including chunk extensions and trailer fields —
the real web needs it, and `example.com` is already one of the sites that answers that way. A
response with neither a `Content-Length` nor chunking is read to the close, and `204`/`304`/`1xx`
and a `HEAD` reply never take a body whatever their headers claim.

What is missing: keep-alive on the *server* side (it still closes after one response, though the
client reuses a connection happily), multipart, cookies, proxies, compression — nothing sends
`Accept-Encoding`, and a server that gzips anyway hands back bytes this does not decode — and a TLS
server side for `httpServe` to sit behind, which is `src/neant/crypto/tls.nt`'s missing half.

`tests/http.nt` is the suite, all of it against sockets this process opens; `tests/data/live-http.nt`
is the one that goes out to the network, run by hand.

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
SHA-256 ~0.22ms/block, ChaCha20-Poly1305 ~22ms per 10KB, X25519 ~48ms — see "Performance" for why
the older figures in this file read faster). 32-bit words live in ints masked after each sum;
the 2^255-19 and 2^130-5 fields use 22- and 26-bit limbs so products stay exact in an int, and
carries run as vector passes.

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
with a different initial hash value and the digest cut to 48 octets, so `sha512Block` is the only
copy of those 80 rounds in the tree and both hashes go through it. It used to live inside
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
roots: x509LoadRoots "/etc/ssl/certs/ca-certificates.crt"        // 146 roots in ~280ms
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
~222ms once for `x509SystemRoots[]`, which `tlsRoots` caches; that one is parsing, not arithmetic,
and is unchanged. Every signature number above was an order of magnitude worse until the bignum
arithmetic became compiled scalar loops (["the bignum arithmetic runs as compiled scalar
loops"](#the-bignum-arithmetic-runs-as-compiled-scalar-loops)); `p256.nt`'s header counts out where
what is left goes and which two optimisations were measured and rejected.

What is still **not** checked, plainly: **P-521**, refused with the curve named, because there is no
`p521.nt`; **SHA-512**, refused by algorithm, because `rsa.nt`'s DigestInfo table stops at SHA-384
and nothing dispatches it; **SHA-1**, refused because it is broken; **Ed25519** in a chain, which
`ed25519.nt` can verify but nothing wires into `verify.nt`'s dispatch; **revocation**, no CRL and no
OCSP, so a certificate revoked this morning still verifies; **name constraints** and certificate
policies; and **extendedKeyUsage**, which is parsed and ignored, so a certificate issued for e-mail
will serve a web request. There are **no client certificates and no TLS server side**. A mismatched
ECDSA algorithm and curve — `ecdsaSha384` under a P-256 key, or the reverse — is also refused, with
both named. Every one of those refusals names the algorithm or the curve rather than skipping the
check. This is a verifier written from scratch to be read, not a substitute for a reviewed TLS
stack, and nothing in it is constant-time.

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

**What it costs.** "No lock" is true and is not the same as free. `Value` held an `Rc` before `spawn`
existed, and crossing a thread needs `Send`, so every refcount became an atomic read-modify-write
instead of a plain increment. Refcount traffic through the operand stack is about 35% of samples
("Performance"), and making that traffic 2–3x dearer per operation costs **~13% of single-threaded
throughput**: the same 64KB SHA-256 measures 196ms at `c49736a` and 221ms at its child `fbdeb99`, the
commit that did the conversion, and is flat from there to today. That is the price of the whole
concurrency story above, paid by every program whether it spawns anything or not. It is not cheaply
recoverable — reverting loses threads and biased refcounting is a pile of `unsafe` — and the lever
that does work is making fewer `Value` clones rather than cheaper ones, which is what the JIT does by
never touching a `Value` at all inside compiled code.

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
one of exactly three shapes — `LoadL(s)` immediately consumed by `Call(1)` (a read), the literal
3-op run `TakeL(s); Amend(1); StoreL(s)` that `iassign` always emits back to back (a write), or a
bare `LoadL(s)` at the very end of the body or immediately before a `Ret` (the vector is the
result, below). Any other appearance disqualifies it; there is no partial typing.

**The access is inlined — no call.** `buf` (`src/jit.rs`) is the locals area followed by one
descriptor word per classified slot, its element count; the slot's own word is the element *data
pointer*. A read is then a load of that count, one unsigned compare that catches a negative index
and an out-of-range one together, and a scaled `ldr` off the slot's own register; a write is the
same with an `str`. Nothing about `Arc`'s layout is assumed, because Rust still does all of it — at
entry `bind_vec` clones the value into the `vecbuf` side table and, for a slot the body *writes*,
calls `Arc::make_mut` on that clone straight away. That is the same copy-on-write
`Op::Amend`/`scatter` (`src/prims.rs`) perform and the same one `jit_vec_set` used to perform on the
first write, only hoisted to entry, which is what makes the elements' address stable for the rest of
the call: a read-only slot is an `Arc` this frame holds a reference to and nothing may mutate, a
written one is uniquely ours and cannot be reallocated. A second live reference to an amended vector
still never sees the write, which `tests/jit.nt` asserts. The trampolines stay in `src/jit.rs` for
the x86-64 backend, which has not been taught this yet.

**A compiled function can return the vector it filled.** Locals are never written back, so until
this everything a compiled body did to a vector was invisible to its caller and the only worthwhile
shape was one that reduces to a scalar — which is why `src/neant/crypto/bignum.nt` could not put a
whole schoolbook product inside one compiled call. Now a body whose last op is a bare `LoadL` of a
classified slot returns that slot's vector: the machine code's x0 is a placeholder and `try_run`
hands back the `Value` the slot was bound to, which the inlined stores have been filling all along.
Every return has to agree — a function returning an int down one path and the vector down another
is refused outright, since there would be no single answer for `src/jit.rs` to believe — and a
vector-returning callee cannot be entered through `jit_call`, whose whole point is that a result is
an int in a register, so that one call deopts.

**The bit builtins are instructions, not calls.** `x band y` is an ordinary two-argument call to a
global holding a `Value::Prim`, so without this every limb mask and every shift inside a compiled
loop was a `blr` through `jit_call` — and limb arithmetic is nothing but masks and shifts.
`src/jit.rs` resolves `badd band bor bxor shl shr` to their global slots, hands them to the codegen,
and records which primitive each slot held; a body that inlined one carries an entry guard that the
slot still holds that exact primitive, because these are ordinary globals and rebinding `band` is
legal. The two shifts deopt unless the count is in [0;64): AArch64 reads it mod 64 where `ib_shl`/
`ib_shr` truncate to u32 and answer 0, so outside that range the machine and the interpreter
genuinely differ. All six work on the raw 64-bit pattern and so may *produce* the int-null sentinel
where `+ - *` may not, and the result is checked for it exactly as an arithmetic one is.

**The virtual operand stack.** A value's physical register used to be its depth — entry *i* lived in
x9+*i*, so a `LoadL` of a local that already had a register of its own still emitted a `mov`, and a
literal always cost a `movz`. Each entry now records where the value actually *is*: a register, or a
constant not yet loaded into one. So a local feeds an add out of its own register, `i+1` is one
`add` with a 12-bit immediate, and a comparison whose result is immediately consumed by a `Jmpf` is
one `cmp` and one conditional branch instead of a `cmp`, a `cset` and a `cbz`. What makes it safe is
that an entry is *materialised* — moved into x9+*i*, where it would always have been — whenever
anything could invalidate it: before every branch and at every branch target, so both halves of a
merge agree where a value lives; before a local's register is written, since an entry aliasing that
register holds the old value; and before a `do` counter is decremented in place. Materialising is a
`mov` the old scheme emitted unconditionally, so the worst case is what it used to cost. On the
measured scalar loop this removes eight instructions of twenty-one and changes nothing at all, which
is itself the finding: that loop is bound by its four branches, not by its instructions. Numbers for
both tiers are together at the end of Stage 2b.

#### The x86-64 backend (compiled, never executed)

Both tiers have a second backend, `src/neant/jit/x86.nt`, for x86-64 System V. Its entry points are
`jitCompileX86` and `jitCompileTraceX86` — the same contracts as the AArch64 pair, named apart so
both files can be in the boot image at once — and `src/jit.rs` picks the pair its target
architecture needs (`CODEGEN`) and is otherwise the same code for both. Everything that decides
*whether* something compiles is called out of `arm64.nt` rather than copied (`jitClassifySlots`,
`jitLocalSlots`, `jitDyadKind`, `jitTraceTouched`, `jitTrPos`, ...), so the compilability walk, the
slot classification and the deopt/guard points are literally the same code and the two backends
accept and reject the same functions and traces. What differs is the encoders, the register
assignment, and how branches are resolved.

The roles the AArch64 file documents map onto this ABI with less room. The long-lived pointers have
to survive the calls a compiled function makes, so they take callee-saved registers: `rbx` the
locals buffer, `rbp` the int-null sentinel, `r12` the `Vm`, `r13` the ok word. The operand stack is
`rsi`, `rdi`, `r8`–`r11` — caller-saved and spilled around every call, as `x9..x14` are — and `rax`
carries the trampoline address for `call rax` and then its result. That leaves `r14`/`r15` for
locals-in-registers (against AArch64's eleven), with the third local and beyond staying in `buf`
exactly as the AArch64 overflow slots do, so no function is rejected for being wide. Because this
ABI's six argument registers *are* four of the operand-stack registers, a call spills the whole
live stack and then loads its arguments back out of those spill slots: there is no order in which
the register moves alone are safe, and this removes the class of bug entirely for a handful of
memory operations on a path that is already making a call. The tracing tier, unlike its AArch64
twin, does need a prologue — six `push`es and six `pop`s at the one tail every exit funnels through
— because nine caller-saved registers cannot hold a buffer pointer, an out pointer, a sentinel, a
scratch, six operand-stack slots and a register per local.

An x86 instruction is variable length, so **every** branch is emitted in its `rel32` form and never
the short `rel8` one, every memory operand uses a full `disp32`, and every constant the 10-byte
`mov r64, imm64`: an instruction's length then depends on its form alone and never on its operands'
values, which is what makes the standard two-pass resolution exact. Pass one emits the instruction
in full with a zero displacement and records the byte offset of that four-byte field; pass two
subtracts and writes four bytes, moving nothing. (The AArch64 file can re-encode a whole branch
word at patch time; here the opcode — and the `test` that sets the flags a conditional branch reads
— is chosen at emission time and only the displacement is left.)

Two places this backend is deliberately narrower, both refusals, so the affected loop just stays
interpreted: **`&`/`|` on floats are not compiled**, because SSE2's `minsd`/`maxsd` return their
second operand when either is a NaN, which is not the IEEE minNum/maxNum that `f64::min`/`max`
(and therefore the interpreter) implement — AArch64 has `FMINNM`/`FMAXNM` and this does not, and
emulating it is a compare, two branches and a NaN case for an operation no measured loop performs.
And the tracing tier has five int local registers against AArch64's eight, so a wider loop body is
not traced. Float `+ - *` and the float comparisons do compile; the comparisons go through
`ucomisd` and the *unsigned* condition codes, because NaN sets ZF, CF and PF together — `x<y` is
`ucomisd y, x` plus `seta` (operands swapped rather than the condition inverted), and `=` needs
`sete` and `setnp` and'ed, which is the one place the integer `&`/`|` verbs being min/max leaves
`and` with a job to do.

In `src/jit.rs` the only architecture-specific parts left are that codegen name and the instruction
cache: `__clear_cache` is declared and called under `#[cfg(target_arch = "aarch64")]` only, because
on x86-64 the caches are coherent and the `mprotect` already orders the write — there is nothing to
do, which is why that is a `cfg` on the call rather than a call to a helper that would be empty on
one target. `MAX_CALL_DEPTH` keeps its AArch64 calibration; the x86-64 frame is smaller (six pushes
and a 72-byte frame, 128 bytes with the return address, against 192), so the same limit is if
anything more conservative there.

**What is verified.** Every encoder is asserted byte for byte against `nasm -f bin` output for the
same mnemonic and operands (`tests/x86.nt`, which says how to re-derive each expectation), and the
emitted code was read back with `ndisasm -b 64` and `llvm-mc --disassemble --triple=x86_64` — the
`llvm-objdump` in this image does not take `-b binary`. The two-pass branch resolution is checked by
computing where a displacement has to land from the encoders' own lengths and reading the four bytes
that were patched in, for a forward jump, a backward jump and a deopt branch. Compilability is
asserted against the AArch64 backend on 25 sources and 10 recorded traces — the accept/reject
verdict, the classified vector slots, and for a trace the whole buffer layout and exits table that
`src/jit.rs` reads back — and the same input twice is required to give byte-identical output.
`cargo check --release --target x86_64-unknown-linux-gnu` passes with no warnings, where before this
work that target compiled 17 dead-code warnings' worth of stubbed-out JIT; that clean build is the
proof the `cfg` work is right.

**What is not.** Nothing this backend emits has ever executed. The development machine is AArch64,
so there is no evidence that the code runs, that the System V details are right in practice (stack
alignment at a `call`, what the trampolines actually preserve), or that a compiled function returns
what the interpreter would. That last one matters most: a wrong register here is a *wrong answer*,
not a crash, and the fail-closed design of both tiers does not help with it — it only guarantees
that what fails to compile falls back. So the first thing to run on an x86-64 box is
`cargo test --release`, whose JIT tests compare every compiled path against the interpreter
(["What the tests check"](#what-the-tests-check)); expect to debug. Every performance number in this
section and the next is AArch64's.

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
- **~200x** — `x[i]` inside a compiled loop (`manual_vector_perf_measurement`). Two reads, a
  multiply and an accumulate per iteration now run at 0.9–1.0ns against 2.7ns when each access was
  a `blr` plus the operand-stack spill around it: level with the scalar loop, which is what an
  inlined bounds check and a scaled load should mean.
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
  miscompiled. These are `tests/jit.nt`, in neant: a case is a source string and the text the REPL
  prints for it, which needs no Rust. What stays in Rust is only what has to look at the host — the
  two wall-clock bounds that catch the JIT silently compiling nothing at all, the `FnCode` flag that
  says a function really was compiled, and the deopt cases that need two separate VMs so one provably
  never tiers up. `tests/bench.nt` is the ratio measurement, run by hand.
- RFC vectors for the crypto and the TLS key schedule (`tests/crypto.nt`); the record layer
  round-trips offline.

What this gives up relative to the oracle: a bug that the compiler introduces *and* reproduces
consistently is no longer caught by construction — it is caught only if a language case exercises it.

#### What the null-sentinel checks cost, measured

Every `+ - *` on two ints emits `cmp` against the null sentinel and a branch, because two ordinary
ints can wrap to exactly it and the interpreter would then propagate a null. In the inner loop of a
schoolbook multiply that is four checks of about thirty instructions. Two ways to make them cheaper
were built and measured against the same loop:

- **One branch per basic block instead of one per op.** Each check increments x4 with `cinc` (no
  branch, no flags left behind) and a single `cbnz` at the end of the straight-line stretch acts on
  the lot, flushed before every branch, call and return so a poisoned value cannot escape the block
  it was made in. Correct, and **12% slower**: the accumulator is a loop-carried dependency where
  the branches were free, being never taken and perfectly predicted. Reverted.
- **Removing them entirely** (unsound — measured only as a ceiling): the multiply-accumulate loop
  goes 0.96ns → 0.80ns per limb product and an RSA-2048 verification 0.50ms → 0.46ms. So perfect
  elimination is worth 17% of the inner loop and **8% of the verification**, which is the budget any
  range analysis on this has to fit inside. It is not where the remaining distance to OpenSSL is.

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

Those absolute figures are **not reproducible on the machine this is developed on** and should be
read as ratios only. `crypto.nt` has not changed since (only the move into `src/neant/crypto/`), and
checking out that same commit here measures SHA-256 at 195ms per 64KB rather than 119ms — so the
numbers above came from different hardware. Measured here today: SHA-256 **220ms** per 64KB
(0.22ms/block), ChaCha20-Poly1305 **22ms** per 10KB, X25519 **48ms**. The 196ms → 221ms between that
commit and now is real, and it is one commit: `fbdeb99`, which converted `Value` from `Rc` to `Arc`
so that `spawn` could exist. Everything after it is flat. See "Concurrency" for the trade — it is the
price of threads, not a regression anyone can take back.

Runtime errors carry a line table, which costs ~10% of compile throughput. A frame is named by its
caller's `LoadG`, so the bytecode carries positions but no names.

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

For TLS, the client now verifies the server *and reaches the public web*: DER, X.509, RSA over
SHA-256 and SHA-384, ECDSA on P-256 and P-384, a trust store, chain validation and hostname matching
are in (`der.nt`, `x509.nt`, `sha512.nt`, `rsa.nt`, `ec.nt`, `p256.nt`, `p384.nt`, `verify.nt`).
Google, Cloudflare, GitHub, Wikipedia, example.com, news.ycombinator.com and Amazon all complete a
verified handshake; the table is in the section above and the test is `the_public_web_verifies`.

Two lessons from getting there, neither the one expected. The first was written down after P-256 and
it held: the README used to say ECDSA P-256 was what stood between this client and Google, and it was
not — their leaves were P-256/SHA-256 and verified, their intermediates were ecdsa-with-SHA384 over
P-384, and the gap was never one algorithm. The second is what closing it actually cost. SHA-384
turned out to be SHA-512's compression function with another IV, so it was a second IV and a
truncation rather than a hash; and P-384 turned out to be a curve record, because `p256.nt` had
already chosen to run on `bignum.nt`'s variable-length limbs rather than fold its own Solinas prime
into fixed-width words. Moving the group law into `ec.nt` and passing the curve as an argument was
the whole of the second curve. The decision that paid for that was made one change earlier, by
measuring an optimisation and declining to write it.

What is missing, then: **P-521**, another curve record and nothing else, wanted by nobody yet;
**SHA-512** in a chain, three table entries in `rsa.nt` and a dispatch line; **Ed25519** in a chain,
which only needs `ed25519.nt` wired into `verify.nt`'s dispatch; **revocation**, which means OCSP or
CRL fetching and so an HTTP client over TLS first — now possible, since the TLS client can reach a
real responder; **name constraints** and **extendedKeyUsage** enforcement; client certificates; and a
server side. Speed is a smaller open item than it was: one P-384 verification is ~38ms and one P-256
~21.5ms against RSA-2048's ~0.50ms, an order of magnitude off each of the old numbers, and what is
left is counted out in ["the bignum arithmetic runs as compiled scalar
loops"](#the-bignum-arithmetic-runs-as-compiled-scalar-loops).

`tlsConnect` used to draw the x25519 private key from `rand`, and then the ClientHello random from it
too. That random goes out in the clear, xorshift64 is linear and invertible, and 32 bytes of it are
enough to solve for the state and roll back to the key — a passive break, no certificate needed. Key
material now comes from `urand`, and `rand` keeps the reproducibility its tests want.

Ed25519 is in (`src/neant/crypto/ed25519.nt`), which took one runtime primitive — `badd` — and no language
change. RSA-PKCS#1 and RSA-PSS followed (`bignum.nt`, `rsa.nt`), on the general modular reduction
that real certificates need — Montgomery, not the special-prime folding the 2^255-19 field gets away
with. RSA *verification* stays cheap because the exponent is 65537: 16 squarings and 2 multiplies,
0.50ms at 2048 bits. ECDSA P-256 (`p256.nt`) went on top of that same bignum arithmetic, over a
curve rather than a modulus, and needed no new primitive either — the one thing it did need was
measuring the special-prime fold before writing it, and finding it level with what was already
there. Not every language addition is code that gets written. SHA-384 and P-384 (`sha512.nt`,
`ec.nt`, `p384.nt`) then needed no new primitive and no new arithmetic at all, only that earlier
measurement having gone the way it did: the hash was an IV and a truncation, and the curve was a
record passed as an argument to the group law P-256 already ran on.
