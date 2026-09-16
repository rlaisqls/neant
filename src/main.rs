//! neant: a vector language. Rust is the VM and the primitives; the front end is neant.
//!
//! q-like syntax (no operator precedence, strict right-to-left) makes compilation a single
//! linear pass; typed vectors make the primitives tight native loops.
//!
//!     x: 1 2 3 4        // vector literal, assignment
//!     2*x+1             // right-to-left: 2*(x+1) -> 4 6 8 10
//!     +/x               // fold -> 10 (fused, no interpreter loop)
//!     {x*y}[3;4]        // lambda, implicit args x y z
//!     $[1<2;`yes;`no]   // cond
//!
//! Source never reaches Rust: src/neant/core/{lex,parse,compile}.nt lex, parse and compile it, and
//! they themselves run on this VM, loaded from the bytecode image in src/neant/image.nb.
mod image;
mod jit;
mod prims;
mod trace;
mod value;
mod vm;

use std::io::{BufRead, Write};

fn report(r: value::R<value::Value>) -> bool {
    match r {
        Ok(v) => { if !matches!(v, value::Value::Null) { prims::out(&v.fmt()); } true }
        Err(e) => { eprintln!("'{}", e.0); false }
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let read = |path: &str| std::fs::read_to_string(path).unwrap_or_else(|e| { eprintln!("{path}: {e}"); std::process::exit(2) });
    let mut vm = boot_vm();
    if argv.get(1).map(String::as_str) == Some("--build-boot") {   // recompile src/neant/{core,stdlib} with the image's own compiler
        let bytes = build_boot_image(&mut vm, &read).unwrap_or_else(|e| { eprintln!("'{}", e.0); std::process::exit(1) });
        std::fs::write(BOOT_IMAGE_PATH, &bytes).unwrap_or_else(|e| { eprintln!("{BOOT_IMAGE_PATH}: {e}"); std::process::exit(2) });
        println!("wrote {BOOT_IMAGE_PATH} ({} bytes); rebuild to embed it", bytes.len());
        return;
    }
    vm.set("args", value::list(argv.iter().skip(2).map(|a| value::chars(a.chars().collect())).collect()));
    if let Some(path) = argv.get(1) {
        if !report(vm.eval(&read(path))) { std::process::exit(1); }
        return;
    }
    let stdin = std::io::stdin();
    let mut line = String::new();
    loop {
        print!("neant) "); std::io::stdout().flush().ok();
        line.clear();
        if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 { println!(); return; }
        report(vm.eval(&line));
    }
}

/// Both JIT backends are in the image: `src/jit.rs` picks the one its target architecture needs
/// (`CODEGEN`), and the other's globals simply never get called.
const BOOT_FILES: [&str; 12] = [
    "src/neant/stdlib/prelude.nt", "src/neant/core/lex.nt", "src/neant/core/parse.nt", "src/neant/core/compile.nt",
    "src/neant/stdlib/table.nt", "src/neant/stdlib/json.nt", "src/neant/stdlib/encode.nt", "src/neant/stdlib/regex.nt",
    "src/neant/stdlib/test.nt", "src/neant/crypto/crypto.nt", "src/neant/jit/arm64.nt", "src/neant/jit/x86.nt",
];
const BOOT_IMAGE_PATH: &str = "src/neant/image.nb";
/// The whole front end and standard library as bytecode, compiled by itself: prelude, lexer, parser,
/// compiler, tables, JSON, crypto. This is the only way into the language — there is no Rust front end,
/// so a broken image can only be rebuilt by a binary carrying a working one (`--build-boot`, or git).
const BOOT_IMAGE: &[u8] = include_bytes!("neant/image.nb");

/// A VM with the embedded boot image loaded — the only way to run neant code.
fn boot_vm() -> vm::Vm {
    let mut vm = vm::Vm::new();
    let img = image::load(BOOT_IMAGE).and_then(|img| vm.exec(&img));
    if let Err(e) = img { eprintln!("boot image: '{}  (rebuild it with a binary that still has a working one)", e.0); std::process::exit(2); }
    vm
}
/// Compile src/neant/{core,stdlib,crypto} with the compiler already in `vm` and serialize the units. Self-hosted: the image
/// that comes out was produced by the image that went in, so a compiler change needs two rebuilds to settle.
fn build_boot_image(vm: &mut vm::Vm, read: &dyn Fn(&str) -> String) -> value::R<Vec<u8>> {
    let ncompile = vm.get("ncompile").ok_or_else(|| value::NError("ncompile: no boot image loaded".into()))?;
    let mut units = Vec::new();
    for f in BOOT_FILES {
        units.extend(vm.call(&ncompile, vec![value::chars(read(f).chars().collect())])?.seq());
    }
    image::dump(&value::pack(units))
}

#[cfg(test)]
mod tests {
    use super::*;
    pub fn ev(vm: &mut vm::Vm, src: &str) -> String { vm.eval(src).map(|v| v.fmt()).unwrap_or_else(|e| format!("'{}", e.0)) }

    /// Every tests/*.nt through tests/run.nt — the language cases (tests/lang.nt), the standard library, the
    /// tables, JSON and the regexes — the same path the file runner takes (cwd is the crate root under
    /// cargo test). run.nt's `exit` would end this whole process, so ntExit: 0b makes it fall through and the
    /// failure count is simply the file's value — smaller than reading a global back, and it is what run.nt returns anyway.
    #[test]
    fn nt_tests() {
        let mut v = boot_vm();
        v.set("args", value::list(vec![]));
        v.eval("ntExit: 0b").unwrap();
        let src = std::fs::read_to_string("tests/run.nt").unwrap();
        let failures = v.eval(&src).unwrap_or_else(|e| panic!("tests/run.nt: '{}", e.0));
        assert_eq!(failures.fmt(), "0", "tests/run.nt reported failures (names are in the captured output above)");
    }

    #[test]
    fn locals_do_not_leak() {
        let mut v = boot_vm();
        v.eval("g:{a:x*2;a+1};g 3").unwrap();
        assert!(v.get("a").is_none());
    }

    #[test]
    fn fused_fold_is_fast() {
        let mut v = boot_vm();
        let t = std::time::Instant::now();
        let r = v.eval("+/{x*x} til 2000000").unwrap();
        assert_eq!(r.fmt(), "2666664666667000000");
        assert!(t.elapsed().as_millis() < 500, "took {:?}", t.elapsed());
    }
}

/// The AArch64 baseline JIT (src/jit.rs): compiles hot integer-only loops after enough calls.
/// `#[cfg(target_arch = "aarch64")]` because that's the only backend — on any other arch
/// `jit::compile` always returns `None` and these functions just run interpreted throughout,
/// which is also exactly what the entry guard falls back to below, so nothing here is arch-specific
/// behavior worth asserting on other architectures.
#[cfg(all(test, target_arch = "aarch64"))]
mod jit_tests {
    use super::*;

    /// `0W + 1` wraps to exactly the null sentinel — the one case raw compiled arithmetic can't
    /// just trust (see src/jit.rs). Run past the tier-up threshold so this specific call is the
    /// compiled version, and check it still lands on the interpreter's answer, proving deopt fired
    /// instead of silently propagating a wrapped garbage value.
    #[test]
    fn deopts_on_null_collision_instead_of_diverging() {
        let mut interpreted = boot_vm();
        let want = tests::ev(&mut interpreted, "p: {[k] n: 0W; i:0; while[i<k; n: n+1; i+:1]; n}; p 3");
        let mut compiled = boot_vm();
        compiled.eval("p: {[k] n: 0W; i:0; while[i<k; n: n+1; i+:1]; n}").unwrap();
        let mut last = String::new();
        for _ in 0..100 { last = tests::ev(&mut compiled, "p 3"); }
        assert_eq!(last, want);
    }

    /// An out-of-range index is a bounds check inside `jit_vec_get`/`jit_vec_set` (src/jit.rs),
    /// not something the codegen can prove away at compile time — same deopt convention as every
    /// other guarded point: bail to the interpreter, which reports it as a real error.
    #[test]
    fn vector_index_out_of_range_deopts_instead_of_diverging() {
        let mut interpreted = boot_vm();
        let want = tests::ev(&mut interpreted, "f: {[x;n] x[n]}; f[1 2 3; 3]");
        let mut compiled = boot_vm();
        compiled.eval("f: {[x;n] x[n]}").unwrap();
        let mut last = String::new();
        for _ in 0..100 { last = tests::ev(&mut compiled, "f[1 2 3; 3]"); }
        assert_eq!(last, want);
    }

    /// Every differential test in this module stays green whether or not a function is ever
    /// compiled — the interpreter is always the fallback, so a codegen that quietly returned `::`
    /// for everything would pass all of them. One test has to notice that a hot function really ran
    /// natively. A `do` loop, because no trace is ever started on one (see `manual_perf_measurement`),
    /// leaving the whole-function JIT as the only thing that can make this finish in the time
    /// asserted: 20M iterations interpret in ~2.3s here and run compiled in ~20ms.
    #[test]
    #[cfg(target_arch = "aarch64")]
    fn a_hot_function_really_is_compiled() {
        let mut v = boot_vm();
        v.eval("f: {[k] n:0; i:0; do[k; n: n+i; i: i+1]; n}").unwrap();
        for _ in 0..70 { v.eval("f 10").unwrap(); } // cross the tier-up threshold
        let t = std::time::Instant::now();
        let r = v.eval("f 20000000").unwrap();
        assert_eq!(r.fmt(), "199999990000000");
        assert!(t.elapsed().as_millis() < 250, "took {:?}", t.elapsed());
    }

    /// Everything above stays correct whether or not a single trace is ever compiled — the
    /// interpreter is always the fallback — so one test has to actually notice that the loop ran
    /// natively. A single call can't reach the whole-function JIT (that needs 64 *calls*), so the
    /// tracing JIT is the only thing that can make this finish in the time asserted: 20M
    /// iterations interpret in ~700ms here and trace in ~40ms.
    #[test]
    #[cfg(target_arch = "aarch64")]
    fn a_hot_loop_really_is_traced() {
        let mut v = boot_vm();
        let t = std::time::Instant::now();
        let r = v.eval("{[k] n:0; i:0; while[i<k; n: n+i; i: i+1]; n} 20000000").unwrap();
        assert_eq!(r.fmt(), "199999990000000");
        assert!(t.elapsed().as_millis() < 250, "took {:?}", t.elapsed());
    }

    /// The same lambda called twice in one iteration is inlined twice, each call site with its
    /// own slot range (so the two bindings of `x` don't clobber each other — `f[i]` is still
    /// live on the stack while `f[i+1]`'s frame runs), but one entry in the entry guard: the
    /// global is checked once, not once per site (`Recorder::enter_frame`).
    #[test]
    fn traced_call_at_two_sites_and_interpreted_agree() {
        let mut v = boot_vm();
        let got = v.eval(
            "f: {x*2}; \
             g: {[k] s:0; i:0; while[i<k; s: s+f[i]+f[i+1]; i: i+1]; s}; \
             gSlow: {[k] s:0; i:0; while[i<k; s: s+(- - f[i])+f[i+1]; i: i+1]; s}; \
             {[k] (g k)=gSlow k} each 63 64 65 200",
        ).unwrap();
        assert_eq!(got.fmt(), "1111b");
    }

    /// The `do` counterpart of `a_hot_loop_really_is_traced`: everything above stays correct
    /// whether or not a `do` loop is ever actually compiled, so one test has to notice that it
    /// was. 20M iterations interpret in ~2.9s here and trace in ~40ms.
    #[test]
    #[cfg(target_arch = "aarch64")]
    fn a_hot_do_loop_really_is_traced() {
        let mut v = boot_vm();
        let t = std::time::Instant::now();
        let r = v.eval("{[k] n:0; i:0; do[k; n: n+i; i: i+1]; n} 20000000").unwrap();
        assert_eq!(r.fmt(), "199999990000000");
        assert!(t.elapsed().as_millis() < 250, "took {:?}", t.elapsed());
    }

    #[test]
    #[ignore]
    fn manual_vector_perf_measurement() {
        let mut v = boot_vm();
        v.eval("f: {[x;y;n] s:0; i:0; while[i<n; s: s+x[i]*y[i]; i: i+1]; s}").unwrap();
        v.eval("xs: til 1000000; ys: til 1000000").unwrap();
        let t0 = std::time::Instant::now();
        v.eval("f[xs;ys;1000000]").unwrap();
        let cold = t0.elapsed();
        for _ in 0..70 { v.eval("f[1 2 3 4 5;1 2 3 4 5;5]").unwrap(); } // cross the tier-up threshold
        let t1 = std::time::Instant::now();
        v.eval("f[xs;ys;1000000]").unwrap();
        let hot = t1.elapsed();
        println!("interpreted {cold:?}  compiled {hot:?}  ratio {:.1}x", cold.as_secs_f64() / hot.as_secs_f64());
    }
}

/// `spawn`/`join`/`shared`/`supd`: real OS threads over Arc-refcounted, copy-on-write values.
/// No global lock — only a `shared` cell takes one, and only for its own critical section.
#[cfg(test)]
mod conc {
    use super::*;

    #[test]
    fn spawn_join_returns_result() {
        let mut v = boot_vm();
        assert_eq!(tests::ev(&mut v, "n: 10; h: spawn {n+1}; join h"), "11");
    }

    #[test]
    fn spawn_join_propagates_error() {
        let mut v = boot_vm();
        assert_eq!(tests::ev(&mut v, "h: spawn {signal \"boom\"}; join h"), "'boom");
    }

    #[test]
    fn join_twice_errors_instead_of_panicking() {
        let mut v = boot_vm();
        assert_eq!(tests::ev(&mut v, "h: spawn {1}; join h; join h"), "'thread: already joined");
    }

    /// 8 threads each doing 2000 `supd` increments on one shared counter: if `supd` ever lost an
    /// update to a race, this would fail intermittently instead of landing on exactly 16000.
    #[test]
    fn shared_cell_serializes_updates() {
        let mut v = boot_vm();
        let got = v.eval(
            "s: shared 0; \
             hs: {spawn {i:0; while[i<2000; supd[s;{x+1}]; i+:1]}} each til 8; \
             {join x} each hs; \
             sget s",
        ).unwrap();
        assert_eq!(got.fmt(), "16000");
    }
}

/// src/neant/crypto/tls.nt is a loadable module, not part of the image: key schedule and record layer,
/// checked offline against RFC 8448. The handshake itself needs a server — see the README.
#[cfg(test)]
mod tls {
    use super::*;
    fn tls_vm() -> vm::Vm {
        let mut v = boot_vm();
        v.eval(&std::fs::read_to_string("src/neant/crypto/tls.nt").unwrap()).unwrap();
        v
    }
    #[test]
    fn key_schedule_matches_rfc8448() {
        let mut v = tls_vm();
        let ev = tests::ev;
        for (src, want) in [
            // RFC 8448 3: PSK and salt both zero
            ("hex hkdfExtract[TLSZ; TLSZ]", "\"33ad0a1c607ec03b09e6cd9893680ce210adf300aa1f2660e1b22e10f170f92a\""),
            ("hex hkdfLabel[32;\"derived\";sha256 0x]",
             "\"00200d746c733133206465726976656420e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\""),
            ("hex deriveSecret[hkdfExtract[TLSZ;TLSZ];\"derived\";0x]",
             "\"6f2615a108c702c5678f54fc9dbab69716c076189c48250cebeac3576c3611ba\""),
            // a traffic secret expands to a 32-byte key and a 12-byte iv
            ("k: trafficKeys hkdfExtract[TLSZ;TLSZ]; (count k[0]; count k[1])", "32 12"),
        ] {
            assert_eq!(ev(&mut v, src), want, "source: {src}");
        }
    }
    #[test]
    fn record_layer_round_trips() {
        let mut v = tls_vm();
        let ev = tests::ev;
        // the sequence number lands in the low 8 bytes of the nonce
        assert_eq!(ev(&mut v, "hex recNonce[12#0x00; 258]"), "\"000000000000000000000102\"");
        // seal then open gives the content type and body back; a wrong sequence number must not open
        let setup = "k: `byte$til 32; iv: 12#0x07; r: sealBody[k;iv;5;23;`byte$\"hello tls\"]; ";
        assert_eq!(ev(&mut v, &format!("{setup}d: openRec[k;iv;5;r[0];wdrop[5;r[1]]]; (d[0]; `char$d[1])")),
                   "(23;\"hello tls\")");
        assert_eq!(ev(&mut v, &format!("{setup}openRec[k;iv;6;r[0];wdrop[5;r[1]]]")), "'aead: bad tag");
    }
    /// hopen/hsend/hrecv/hclose against a listener in this test's own process.
    #[test]
    fn sockets_round_trip() {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let srv = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut b = [0u8; 64];
            let n = s.read(&mut b).unwrap();
            s.write_all(&[b"echo:", &b[..n]].concat()).unwrap();
        });
        let mut v = boot_vm();
        v.set("port", value::chars(port.to_string().chars().collect()));
        let got = v.eval("h: hopen \"127.0.0.1:\",port; hsend[h;\"hi there\"]; r: hrecv[h;64]; hclose h; `char$r").unwrap();
        assert_eq!(got.fmt(), "\"echo:hi there\"");
        srv.join().unwrap();
    }
    /// The whole point of `accept` handing connections to `spawn`ed workers: one worker blocked
    /// on `hrecv` (client A, deliberately never sent) must not stall the others. If the global
    /// socket table (`SOCKS`, src/prims.rs) held its lock across blocking I/O instead of cloning
    /// the fd and releasing it first, B and C's `hsend`/`hrecv` would hang behind A's — instead
    /// they're asserted to finish, with A still pending, before A is ever unblocked.
    #[test]
    fn accept_hands_connections_to_independent_spawned_workers() {
        use std::io::{Read, Write};
        let mut v = boot_vm();
        // src/prims.rs keeps no way to ask a listener its bound port (out of scope, see README) —
        // the test bypasses that by binding its own listener on an OS-assigned port, dropping it
        // immediately, and pointing neant's `hlisten` at that same port. A small, accepted TOCTOU
        // race in exchange for not growing the primitive surface just for this test.
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        v.eval(&format!("l: hlisten \"127.0.0.1:{port}\"")).unwrap();
        let server = std::thread::spawn(move || {
            v.eval(
                "i:0; while[i<3; c: accept l; spawn {x: hrecv[c;1]; hsend[c;\"ok\\n\"]; hclose c}; i+:1]; hclose l",
            ).unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(100)); // let the accept loop start
        let mut a = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        for _ in 0..2 {
            let tx = tx.clone();
            let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            std::thread::spawn(move || {
                c.write_all(b"x").unwrap();
                let mut buf = [0u8; 3];
                c.read_exact(&mut buf).unwrap();
                tx.send(&buf == b"ok\n").unwrap();
            });
        }
        // B and C must both complete while A is still blocked on its own hrecv.
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)), Ok(true));
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)), Ok(true));
        a.write_all(b"x").unwrap();
        let mut buf = [0u8; 3];
        a.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ok\n");
        server.join().unwrap();
    }
    #[test]
    fn client_hello_is_well_formed() {
        let mut v = tls_vm();
        let got = v.eval("ch: clientHello[\"a.b\"; 32#0x01; 32#0x02; 32#0x03]; (count ch; hex 6#ch; hex ch[71+til 5])").unwrap();
        // 0x01 ClientHello, 24-bit length, then 0x0303; cipher_suites is the one suite 0x1303
        assert_eq!(got.fmt(), "(160;\"0100009c0303\";\"0002130301\")");
    }
}

/// src/neant/net/http.nt is a loadable module, not part of the image: request parsing and response writing
/// over hlisten/accept, one spawned worker per connection (httpServe).
#[cfg(test)]
mod http {
    use super::*;
    use std::io::{Read, Write};

    /// httpServe run against real client sockets (not neant's own hopen), the most direct way to
    /// check request parsing and response formatting against literal bytes on the wire.
    #[test]
    fn serves_get_and_post_over_real_sockets() {
        let mut v = boot_vm();
        v.eval(&std::fs::read_to_string("src/neant/net/http.nt").unwrap()).unwrap();
        // src/prims.rs has no way to ask a listener its bound port — bind a probe in Rust to grab
        // a free one, drop it, and point neant's hlisten at that same port (a small, accepted
        // TOCTOU race, same trick as the accept-workers test above).
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        v.eval(&format!("l: hlisten \"127.0.0.1:{port}\"")).unwrap();
        v.eval(
            "handler: {[req] (200;\"OK\";(`$\"content-type\")!(,\"text/plain\"); \
             \"method=\",req[`method],\" path=\",req[`path],\" body=\",req[`body])}",
        ).unwrap();
        std::thread::spawn(move || v.eval("httpServe[l;handler]").unwrap());
        std::thread::sleep(std::time::Duration::from_millis(100));

        let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"GET /hello HTTP/1.1\r\nHost: h\r\n\r\n").unwrap();
        let mut resp = String::new();
        c.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 200 OK\r\n"), "{resp:?}");
        assert!(resp.contains("content-type: text/plain\r\n"), "{resp:?}");
        assert!(resp.ends_with("method=GET path=/hello body="), "{resp:?}");

        let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let body = "hi there";
        c.write_all(format!("POST /echo HTTP/1.1\r\nHost: h\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).unwrap();
        let mut resp = String::new();
        c.read_to_string(&mut resp).unwrap();
        assert!(resp.ends_with("method=POST path=/echo body=hi there"), "{resp:?}");
    }
}

/// src/neant/crypto/ed25519.nt is a loadable module, not part of the image: SHA-512 on raw 64-bit words, and
/// Ed25519 verification on src/neant/crypto/crypto.nt's 2^255-19 field. FIPS 180-4 and RFC 8032 vectors.
#[cfg(test)]
mod ed25519 {
    use super::*;
    fn ed_vm() -> vm::Vm {
        let mut v = boot_vm();
        v.eval(&std::fs::read_to_string("src/neant/crypto/ed25519.nt").unwrap()).unwrap();
        v
    }
    const V: [(&str, &str, &str); 4] = [
        ("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a", "",
         "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"),
        ("3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c", "72",
         "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00"),
        ("fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025", "af82",
         "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a"),
        ("ec172b93ad5e563bf4932c70e1245034c35467ef2efd4d64ebf819683467e2bf",
         "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
         "dc2a4459e7369633a52b1bf277839a00201009a3efbf3ecb69bea2186c26b58909351fc9ac90b3ecfdfbc7c66431e0303dca179c138ac17ad9bef1177331a704"),
    ];
    #[test]
    fn rfc8032_signatures_verify() {
        let mut v = ed_vm();
        for (pk, m, sg) in V {
            let src = format!("ed25519Verify[unhex \"{pk}\"; unhex \"{m}\"; unhex \"{sg}\"]");
            assert_eq!(tests::ev(&mut v, &src), "1b", "RFC 8032 vector {pk}");
        }
    }
    /// Everything that must not verify. The last one is a signature with S = L: the point equation
    /// still holds, so only the canonicality check rejects it.
    #[test]
    fn forgeries_are_rejected() {
        let mut v = ed_vm();
        let (pk, m, sg) = V[1];
        let (other, _, _) = V[2];
        for (what, src) in [
            ("flipped signature bit", format!("s: unhex \"{sg}\"; ed25519Verify[unhex \"{pk}\"; unhex \"{m}\"; (63#s),bnot s[63]]")),
            ("wrong message", format!("ed25519Verify[unhex \"{pk}\"; unhex \"73\"; unhex \"{sg}\"]")),
            ("wrong public key", format!("ed25519Verify[unhex \"{other}\"; unhex \"{m}\"; unhex \"{sg}\"]")),
            ("truncated signature", format!("ed25519Verify[unhex \"{pk}\"; unhex \"{m}\"; 63#unhex \"{sg}\"]")),
            ("public key not on the curve", format!("ed25519Verify[32#0xff; unhex \"{m}\"; unhex \"{sg}\"]")),
            ("S = L, not canonical", format!("ed25519Verify[unhex \"{pk}\"; unhex \"{m}\"; (32#unhex \"{sg}\"),unhex \"edd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010\"]")),
        ] {
            assert_eq!(tests::ev(&mut v, &src), "0b", "accepted a forgery: {what}");
        }
    }
}

#[cfg(test)]
mod boot {
    use super::*;

    /// With the Rust front end gone there is no external oracle, so the corpora below are checked for
    /// *stability* instead: the front end must lex, parse and compile them to exactly the same thing
    /// after it has been recompiled by itself. A compiler that does not reproduce its own output on
    /// these fails here; `language` and `embedded_boot_image_is_current` cover what the output means.
    const LEX_CORPUS: &[&str] = &[
        "x: 1 2 3; 2*x+1",
        "1 -2 - 3 -.5 1e3 2.5e-1 7.",
        "f:{[a;b] a-b}; f[10;3]",
        "`a`b!1 2 // comment\nd`b; `; `a.b_c",
        "\"str\\n\\\"q\\\"\" , \"a\" , \"\"",
        "+/1 2 3 4;+\\x;{x*x}'1 2 3",
        "$[1<2;`yes;`no];(1;`a;\"s\")",
        "x[0]:9;n+:1;{5}[];f[;3] 10; :5",
        "a_1;1_a;-1_a;x-1;x -1;(x)-1",
        "if[i<n; v[i]: i*i; i+:1]\n\twhile[0;]",
        "101b 1b 0N 0W -0W 0n 0w 1 0N 3 2.5 0n", "2026.09.15 12:30:00 12:30:00.25 9:05 2026.13.01 12:30",
        "2026.01.01 2026.01.03 12:00:00 2026.01.01 5", ".ns.v .z.d x.y 1.5.2",
        "x +\\: y; x +/: y; f\\:[a;b]; \"a\\nb\" \\ 5", "0x0aff 0x0a 0x; x: 0x01,0xAB",
    ];
    const PARSE_CORPUS: &[&str] = &[
        "x: 1 2 3; 2*x+1", "1 -2 - 3 -.5", "f:{[a;b] a-b}; f[10;3]", "`a`b!1 2 // c\nd`b",
        "\"str\" , \"a\" , \"\"", "+/1 2 3 4;+\\x;{x*x}'1 2 3;(+/) 1 2;f/", "$[1<2;`yes;`no];(1;`a;\"s\");(1;2)",
        "x[0]:9;n+:1;x,:5;g::7;{5}[];f[;3] 10;f[1;];f[]", ":5;{if[x<0; :`neg]; `pos}[-1]",
        "if[i<n; v[i]: i*i; i+:1]\n\twhile[0;]", "{x+y*z};{y};{};{[] 1};{{x} each y}",
        "a_1;1_a;-1_a;x-1;x -1;(x)-1;x mod 3;7 in 1 2;(f each x) over y",
        "d: `a`b!1 2\nd[`c]: 3\nf: {[s;i] $[i<count s; s[i]; \"\"]}\n", "()", ";;", "", "print \"hi\"",
        "x[1;0]:9; c[1]+:10; do[5;n+:2]; x +\\: y; 2026.09.15+1; 101b", "while[1;break]; do[3;if[x;break]]",
        "select a, sum b by c, d:e+1 from t where x>1, y<2", "select from t where a>1", "select total: sum b from t", "select sum v by k from t",
        "{n:1;{x+n}}; {a:1;b:{c:2;{a+c+x}};b[][10]}",
    ];
    const COMPILE_CORPUS: &[&str] = &[
        "x: 1 2 3; 2*x+1", "f:{[a;b] a-b}; f[10;3]", "+/1 2 3 4;+\\x;{x*x}'1 2 3;(+/) 1 2;f/", "$[1<2;`yes;`no];(1;`a;\"s\");$[0;1]",
        "x[0]:9;n+:1;x,:5;g::7;{5}[];f[;3] 10", ":5;{if[x<0; :`neg]; `pos}[-1]", "if[i<n; v[i]: i*i; i+:1]\n\twhile[0;]",
        "{x+y*z};{y};{};{[] 1};{{x} each y};{a:1;b:a+x;a[0]:b;g::a;{c:1;a};b}", "x mod 3;(f each x) over y;{x[i]:1}",
        "fact:{$[x<2;1;x*fact x-1]}", "", ";;", "()",
        "f:{n:10;{x+n}}; add:{[a] {[b] a+b}}; h:{a:1;b:{c:2;{a+c+x}};b[][10]}; {n:1;g:{n};n:2;g[]}",
        "x:(1 2;3 4);x[1;0]:9; c[1]+:10; n:0;do[5;n+:2]; 1 2 3 +\\: 10 20; select sum v by k from t where v>1", "n:0;while[1;n+:1;if[n>4;break]];do[3;break]",
    ];

    /// Every corpus through the stage it belongs to, as one value the two generations can be compared on.
    fn front_end_output(v: &mut vm::Vm) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (stage, corpus) in [("nlex", LEX_CORPUS), ("nparse", PARSE_CORPUS), ("ncompile", COMPILE_CORPUS)] {
            for src in corpus {
                v.set("src", value::chars(src.chars().collect()));
                let got = v.eval(&format!("{stage} src")).map(|r| r.fmt()).unwrap_or_else(|e| format!("'{}", e.0));
                out.push((format!("{stage} {src:?}"), got));
            }
        }
        out
    }

    /// Generation 2: recompile every boot file through the pipeline it defines, then it must lex, parse and
    /// compile the corpora byte-identically and still pass every tests/*.nt. This is what the Rust oracle
    /// used to check — a front end that changes its own output when rebuilt by itself now fails here.
    #[test]
    fn front_end_reproduces_itself() {
        let mut v = boot_vm();
        let gen1 = front_end_output(&mut v);

        let t = std::time::Instant::now();
        for f in BOOT_FILES {
            v.eval(&std::fs::read_to_string(f).unwrap()).unwrap_or_else(|e| panic!("{f}: '{}", e.0));
        }
        let rebuild = t.elapsed();

        for ((what, a), (_, b)) in gen1.iter().zip(front_end_output(&mut v)) {
            assert_eq!(*a, b, "gen2 differs on {what}");
        }
        v.set("args", value::list(vec![]));
        v.eval("ntExit: 0b").unwrap();
        let failures = v.eval(&std::fs::read_to_string("tests/run.nt").unwrap())
            .unwrap_or_else(|e| panic!("gen2 tests/run.nt: '{}", e.0));
        assert_eq!(failures.fmt(), "0", "gen2 failed tests/*.nt (the names are in the captured output above)");
        assert!(rebuild.as_millis() < 3000, "self-rebuild took {rebuild:?}");
        eprintln!("self-hosted rebuild of src/neant/{{core,stdlib,crypto}}: {rebuild:?}");
    }

    /// Lexer and parser errors name the stage and the line. These messages are the front end's only
    /// remaining contract that no other test pins down.
    #[test]
    fn front_end_errors_carry_positions() {
        let mut v = boot_vm();
        for (src, msg) in [
            ("1+\"x", "lex: unterminated string at line 1"), ("\"ab\n\nc", "lex: unterminated string at line 1"),
            ("1+", "parse: incomplete expression at line 1"), ("(1", "parse: missing ) at line 1"),
            ("x:", "parse: empty assignment at line 1"), ("f[1", "parse: missing ] at line 1"),
            ("1\n2\n(", "parse: missing ) at line 3"), ("select a", "parse: select needs from at line 1"),
            ("}", "parse: unexpected } at line 1"), ("(1;2}", "parse: unexpected } at line 1"), ("f[1]]", "parse: unexpected ] at line 1"),
        ] {
            v.set("src", value::chars(src.chars().collect()));
            assert_eq!(super::tests::ev(&mut v, "nparse src"), format!("'{msg}"), "{src:?}");
        }
    }
}

#[cfg(test)]
mod boot_image {
    use super::*;
    #[test]
    fn image_roundtrip() {
        let mut v = boot_vm();
        let x = v.eval("(1;2.5;`a;`b`c;\"s\";\"str\";1 2 3;1.5 2.5;1=1 0;(();`k`j!1 2);$[0;0])").unwrap();
        assert!(image::load(&image::dump(&x).unwrap()).unwrap() == x, "{}", x.fmt());
        assert_eq!(image::load(b"").unwrap_err().0, "image: empty");
    }
    /// The image must be a fixpoint: compiling the current boot sources with it reproduces it exactly.
    /// This fails both when the sources under src/neant/{core,stdlib} have moved ahead of
    /// src/neant/image.nb and when a compiler change has only been rebuilt once — run
    /// `cargo run --release -- --build-boot` (twice, after a compiler change) and rebuild.
    #[test]
    fn embedded_boot_image_is_current() {
        let read = |p: &str| std::fs::read_to_string(p).unwrap();
        let mut vm = boot_vm();
        let fresh = build_boot_image(&mut vm, &read).unwrap();
        assert!(fresh == BOOT_IMAGE, "src/neant/image.nb is stale ({} vs {} bytes): run `cargo run --release -- --build-boot` and rebuild", BOOT_IMAGE.len(), fresh.len());
    }
}

