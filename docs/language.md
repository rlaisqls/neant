# The language

Reference. The front door is [../README.md](../README.md).

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

## Encodings

`src/neant/stdlib/encode.nt`: base64 and percent-encoding on the byte vectors `` `byte$ `` gives, and query strings.

```
b64 `byte$"foobar"           // "Zm9vYmFy"    unb64 "Zm9vYmFy" -> 0x666f6f626172;  b64url/unb64url are the - _ unpadded form JWTs use
urlenc "a b/é"               // "a%20b%2F%C3%A9"   RFC 3986: unreserved chars pass, every other UTF-8 byte is %XX
urldec "a%20b+c"             // "a b c"       + reads as a space, like a form
qparse "a=1&b=x+y"           // `a`b!("1";"x y")     qbuild inverts it
```

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

## Gotchas (shared with q)

- `i+1<n` is `i+(1<n)`. Write `(i+1)<n`. Every comparison inside arithmetic needs parens.
- `string +/v` is `+/` applied dyadically to `string` and `v`. Write `string sum v` or `string (+/)v`.
- A glued `-` after a noun is subtraction: `f -1` is `f - 1`; write `f[-1]`.
- A name followed by a verb is that verb's left argument: `til #p` is `til # p` (take), `value =x` is `value = x`. Write `til count p`, `value group x`. When the name holds a *function* this used to build a silent two-element list — `f ,x` is now an error that says so.
- `in` against a plain string is per character: `"ab" in "abc"` is `11b`, not a substring test — use `ss` for that. Against a *list* of strings it does match whole strings, so `"from" in ("by";"from")` is `1b`.
- Closures capture by value; assigning a captured name inside the inner lambda makes it a new local (like q). No mutable counters.
- A variable assigned anywhere in a lambda is local to it. `x::v` assigns the global. A local cannot be named after an infix verb (`in`, `sv`, `cut`, `bin`, …) — the parser would read it as the verb, so the compiler rejects it by name.
- A newline ends a statement, so an expression cannot be split across lines. Build it up with `,:` instead.

