# Library

Tables, JSON, regexes, tooling, HTTP, TLS, concurrency. Reference; the front door is [../README.md](../README.md).

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

`httpServe` serves requests on a connection until either side is done with it: HTTP/1.1 keeps it
open, 1.0 closes unless it asks not to, `Connection: close` from either the request or the handler
ends it, and so does end of stream or a malformed request. A reply on a connection that stays open
has to say where it ends, so `httpSend` fills in a `Content-Length` the handler did not set. What
makes any of this work is that a connection carries a `pending` buffer: finding the end of a header
block means reading past it, and those bytes belong to the next message — without somewhere to put
them a second request loses its first bytes, which is the same reason the client can reuse a
connection at all.

Chunked transfer-encoding is decoded on both sides, including chunk extensions and trailer fields —
the real web needs it, and `example.com` is already one of the sites that answers that way. A
response with neither a `Content-Length` nor chunking is read to the close, and `204`/`304`/`1xx`
and a `HEAD` reply never take a body whatever their headers claim.

`httpsServe[l; handler; certs; pk]` is the same server behind TLS — the transport triple swapped for
`tlsAccept`'s and nothing else changed:

```
load "src/neant/crypto/tlsserver.nt"; load "src/neant/crypto/sign.nt"
l: hlisten "0.0.0.0:8443"
httpsServe[l; handler; pemLoad "cert.pem"; rsaKeyLoad "key.pem"]
```

`curl --tlsv1.3 --cacert root.pem https://leaf.neant.test:8443/from-curl` fetches from it with the
certificate verified (`ssl_verify_result 0`).

What is missing: pipelining (a request is answered before the next is read), multipart, cookies,
proxies, and compression — nothing sends `Accept-Encoding`, and a server that gzips anyway hands
back bytes this does not decode.

`tests/http.nt` is the suite, all of it against sockets this process opens; `tests/data/live-http.nt`
is the one that goes out to the network, run by hand.

## Serving TLS

`src/neant/crypto/tlsserver.nt` is the handshake from the other side: `tlsAccept[conn; certs; pk]`
returns an ordinary `tls.nt` handle, so `tlsSend`, `tlsRecv` and `tlsClose` work on it unchanged —
the only thing that differs between the two ends is which traffic secret writes and which reads.
Everything else is `tls.nt`'s functions called in mirror: the record layer, the key schedule, the
transcript discipline. What is new is the ClientHello parser, the ServerHello builder, and a
CertificateVerify **signed** rather than checked, which is what `src/neant/crypto/sign.nt` exists for.

The scope is narrow and deliberate: one cipher suite (`TLS_CHACHA20_POLY1305_SHA256`, because
`crypto.nt` has ChaCha20 and Poly1305 and no AES), one group (x25519), one signature scheme
(`rsa_pss_rsae_sha256` for an RSA key, `ecdsa_secp256r1_sha256` for a P-256 one — the server signs
with whichever kind of key it was handed, and a key on any other curve is refused with the curve
named). No client certificates — it never sends a
CertificateRequest, so whoever connects is anonymous. No resumption, no early data, no
HelloRetryRequest: a client whose key_share is not x25519 is refused rather than asked to try again.
Each of those refusals names what was missing.

The test that matters is not this process talking to itself: `openssl s_client` handshakes against
it, verifies the fixture chain and echoes a line back (`openssl_client_completes_our_handshake`,
src/main.rs), so a ServerHello, a signature or a Finished got subtly wrong fails there rather than
only in a conversation with ourselves. **`sign.nt` is only partly constant-time and a server is where
that matters most**: nothing in it branches on a secret any more (`ecCtMul` and `bnModExpCtL`,
below), but the base is not blinded — read its header before putting this anywhere real.

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
("Performance"), and making that traffic 2–3x dearer per operation costs **~13% of interpreted
throughput**: a 64KB SHA-256 through the interpreter measures 196ms at `c49736a` and 221ms at its
child `fbdeb99`, the commit that did the conversion. Every program pays it, whether it spawns
anything or not, and it is not cheaply recoverable — reverting loses threads and biased refcounting
is a pile of `unsafe`.

The lever that does work is making *fewer* `Value` clones rather than cheaper ones, and that is no
longer an argument: the same SHA-256 is **1.0ms** today, because its inner loop became compiled code
that touches no `Value` at all (["The hash and the stream cipher run as compiled scalar
loops"](#the-hash-and-the-stream-cipher-run-as-compiled-scalar-loops)). The 13% is a tax on the
interpreter, so it is paid in full by everything the JIT does not take and not at all by what it
does — which is why that benchmark can no longer be used to measure it.

