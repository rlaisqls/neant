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
"," vs "a,b"        // ("a";"b")   strings: vs sv ss upper lower trim, `int$"42" `char$65
read0 "f.txt"       //             IO: read0 write0 print args
{if[x<0; :`neg]; `pos}              // :x returns early from a lambda
counter::0; {counter::counter+1}[]  // :: assigns a global from inside a lambda
1 -2                // vector 1 -2   (glued minus is a literal; `1 - 2` subtracts)
```

Verbs: `+ - * % & | < > = ~ , # _ ! ? @ ^ $` (each monadic and dyadic, q meanings).
Adverbs: `/` fold, `\` scan, `'` each. Control: `if[c;...]`, `while[c;...]`, `$[c;a;b;...]`.
Standard library (`boot/prelude.nt`, written in neant): `sum avg min max count first last til sort distinct raze
reverse where sqrt floor string sym type not mod div xexp in within vs sv ss lower upper trim
neg ceiling round signum med var dev rank xbar bin any all msum mavg mmax mmin ema rotate cut sublist differ ltrim rtrim ssr like hex unhex`.
Rust builtins, only what needs the host: `exp log sin cos tan atan rand rseed band bor bxor shl shr bnot key value group show print signal exit read0 write0 each over scan`.
`n rand m` draws n from [0;m) or from the list m; `rseed 7` makes a run reproducible.
Tables (`boot/table.nt`, in neant — a table is a dict of columns): `tbl row rows tsel tsort tby tappend tcount tshow`. Joins: `lj ij uj aj`.
JSON (`boot/json.nt`): `jk "{\"a\": [1, 2]}"` parses (objects are dicts, null is `::`), `jj x` serializes.
Bytes: `0x0aff` literals, `` `byte$"hé" `` UTF-8 encodes, `` `char$0x68c3a9 `` decodes, `` `int$0x0aff `` is `10 255`; arithmetic on bytes gives ints,
the bit verbs on two byte operands give bytes (`key bxor data`). `boot/crypto.nt`, pure neant on those: `sha256 hmac hkdfExtract hkdfExpand chacha20 poly1305 aeadEncrypt aeadDecrypt x25519`,
all checked against the RFC vectors (SHA-256 ~0.3ms/block, ChaCha20-Poly1305 ~25ms per 10KB, X25519 ~75ms). 32-bit words live in ints
masked after each sum; the 2^255-19 and 2^130-5 fields use 22- and 26-bit limbs so products stay exact in an int, and carries run as
vector passes. What TLS 1.3 still lacks is only the handshake state machine, X.509, and a socket.

```
t: tbl[`name`dept`pay; (`ann`bob`cy; `eng`ops`eng; 120 80 100)]
tsel[t; t[`pay]>90]          // rows where
tby[t;`dept;`pay;sum]        // `eng`ops!220 80
tshow tsort[t;`pay]          // aligned grid
```

## Gotchas (shared with q)

- `i+1<n` is `i+(1<n)`. Write `(i+1)<n`. Every comparison inside arithmetic needs parens.
- `string +/v` is `+/` applied dyadically to `string` and `v`. Write `string sum v` or `string (+/)v`.
- A glued `-` after a noun is subtraction: `f -1` is `f - 1`; write `f[-1]`.
- A name followed by a verb is that verb's left argument: `til #p` is `til # p` (take), `value =x` is `value = x`. Write `til count p`, `value group x`.
- `in` on a string is per character: `"from" in ("by";"from")` is 0000b. Match whole strings with `~/:`: `|/ "from" ~/: kws`.
- Closures capture by value; assigning a captured name inside the inner lambda makes it a new local (like q). No mutable counters.
- A variable assigned anywhere in a lambda is local to it. `x::v` assigns the global.

## Run

```
cargo run --release                     # REPL (self-hosted front end from the embedded boot image)
cargo run --release -- file.nt          # run a file
cargo run --release -- --rust file.nt   # same, through the Rust lexer/parser/compiler (debugging aid)
cargo test --release

After editing boot/*.nt or the compiler: `cargo run --release -- --build-boot`, then rebuild — `cargo test`
fails until the embedded boot/boot.nb matches the sources again.
```

## Plan

Stage 0 (done): lexer, parser, compiler, VM in Rust — `src/{lex,parse,compile,vm,prims,value}.rs`.
Stage 1 (in progress): rewrite lex/parse/compile in neant and run them on this VM, PyPy-style; the Rust
VM and primitives stay as the runtime. `boot/lex.nt` is the lexer — `cargo test` checks it against the
Rust lexer (exposed as the `lex` builtin) token-for-token. `boot/parse.nt` is the parser: nodes are
`(`kind; ...)` lists with identifiers as symbols, checked against the Rust parser (`parse` builtin) on the
same corpus. `boot/compile.nt` is the compiler: it emits bytecode as data — a unit is `(opcodes; args; consts)`,
consts are tagged `(`k;v)` `(`g;`name)` `(`p;"+")` `(`a;"/";f)` `(`f;code)` — checked against the Rust compiler
(`compile` builtin), and `exec` loads and runs it. `nrun src` is the whole pipeline with no Rust front end:
`cargo test` runs every language case through it, then rebuilds the boot files with themselves (generation 2)
and checks they still match the oracle. `--build-boot` serializes that bytecode into `boot/boot.nb`
(`src/image.rs`), which is embedded in the binary: **the default front end is neant compiled by neant**; Rust
is the VM plus primitives (and a stage-0 front end kept for building the image and as a test oracle).
Boot compiler speed: `x,: y` compiles to Take+join so appends are in place (20k appends 444ms -> 1ms), globals are
interned to slots at load, execution stacks are pooled: the 7.7KB parser lexes+parses+compiles itself in ~100ms
(was ~200ms). `?` `distinct` `group` hash atoms (200k ints/1000 keys: distinct 159ms -> 6ms, group 198ms -> 10ms);
nested keys fall back to a scan. Next: a register-style calling convention (calls are ~100ns, the boot parser
makes ~100 per token), and moving more of the VM dispatch into neant-generated specialised code.

## More syntax and values

```
101b  0N 0W  0n 0w                 // bool literals; int null/infinity (0N propagates through + - *); float null/inf
2026.09.15 + 30                    // dates (days since 2000.01.01): 2026.10.15;  d1-d2 -> days;  `year$ `month$ `day$
12:30:00.250 + 1000                // times (ms since midnight);  `hour$ `minute$ `second$;  today[]  now`time
`date$"2026.02.28"  `int$d         // casts both ways; isnull x; fill[0;x]; fills x
f: {n: 10; {x+n}}; (f 0) 5         // closures capture enclosing locals by value -> 15
x[1;0]: 9   c[1]+: 10   do[5; ..]  // deep index assignment, compound index assignment, do loop; break leaves while/do
2026.01.01 2026.01.03              // date vector literal
1 2 3 +\: 10 20   1 2 3 +/: 10 20  // each-left / each-right
.ns.name: 7                        // dotted names as namespaces; load "file.nt" runs a file
select total: sum pay, n: count pay by dept from t where pay>90
                                   // q-style select, desugared by the parser into qsel[t;where;by;cols] (boot/table.nt)
lj[t;`k;u]  ij[t;`k;u]  uj[t;u]    // joins on key columns; ungroup t; rcsv["SSI";",";"file.csv"]
t[1]  t[0 2]  t[where t[`pay]>90]  // rows of a table by position
kt: xkey[`id;t]; kt 3; unkey kt    // keyed table: key rows -> remaining columns; kt[(1;2)] for a multi-column key
(1 2;3 4)?3 4                      // ? on a general list finds a whole row (1); so does `in`
deltas prev next sums prds maxs mins ratios asc desc except inter union cross fmt
'parse: missing ) at line 3        // lexer and parser errors carry the line
```
