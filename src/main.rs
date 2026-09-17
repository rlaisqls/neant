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
        // 0x01 ClientHello, 24-bit length, then 0x0303; cipher_suites is the one suite 0x1303.
        // 166 bytes: signature_algorithms is six entries now that src/neant/crypto/{p384,sha512}.nt
        // can check ECDSA-SHA384 on P-384 and RSA over SHA-384, so the SHA-384 schemes are offered
        // beside the SHA-256 ones. Every scheme in the list is one verify.nt can verify; the list
        // is asserted in full below.
        assert_eq!(got.fmt(), "(166;\"010000a20303\";\"0002130301\")");
        // the signature_algorithms extension (13) in full, most preferred first: 1027
        // ecdsa_secp256r1_sha256 and 1283 ecdsa_secp384r1_sha384 -- between them what the public
        // web serves -- then 2052 rsa_pss_rsae_sha256 and 2053 rsa_pss_rsae_sha384, any of which
        // signs the CertificateVerify, and finally 1025 rsa_pkcs1_sha256 and 1281 rsa_pkcs1_sha384,
        // which only ever sign a certificate.
        // The extensions start at 79: 4 bytes of handshake header, version(2), random(32),
        // a 32-byte session_id behind its length byte, cipher_suites(2+2), compression(1+1) and
        // the extensions' own 2-byte length
        assert_eq!(tests::ev(&mut v, "hex findExt[wdrop[79;ch]; 13]"), "\"000c040305030804080504010501\"");
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
            // `body` is bytes (http.nt: a request body may be binary), so `char$ it to concatenate
            "handler: {[req] (200;\"OK\";(`$\"content-type\")!(,\"text/plain\"); \
             \"method=\",req[`method],\" path=\",req[`path],\" body=\",`char$req[`body])}",
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

/// src/neant/crypto/ed25519.nt is a loadable module, not part of the image, over sha512.nt's SHA-512 on raw 64-bit words, and
/// Ed25519 verification on src/neant/crypto/crypto.nt's 2^255-19 field. FIPS 180-4 and RFC 8032 vectors.
#[cfg(test)]
mod ed25519 {
    use super::*;
    fn ed_vm() -> vm::Vm {
        let mut v = boot_vm();
        // SHA-512 moved out of ed25519.nt into sha512.nt when SHA-384 needed the same compression
        // function; ed25519.nt's header says so, and this is a caller loading both.
        for f in ["src/neant/crypto/sha512.nt", "src/neant/crypto/ed25519.nt"] {
            v.eval(&std::fs::read_to_string(f).unwrap()).unwrap_or_else(|e| panic!("{f}: '{}", e.0));
        }
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

/// RSA signature verification, on the general modular arithmetic in src/neant/crypto/bignum.nt.
/// Nothing here is in the boot image, so both files are loaded into a plain boot VM. Every vector
/// was made on this machine with the command quoted above it and can be regenerated from it; the
/// key is throwaway and its private half only ever existed to forge the blocks that must NOT verify.
/// src/neant/crypto/{der,x509}.nt are loadable modules, not part of the image: ASN.1 DER and X.509
/// certificate parsing — the half of certificate verification that establishes what a certificate
/// *says*, with no signature check, no chain building and no hostname match anywhere in it.
///
/// Every expected value below was read off `openssl x509 -text -noout` (OpenSSL 3.6.2) for the same
/// fixture, and each fixture under tests/data/ carries the command that generated it in its own
/// header. The hand-written DER vectors are X.690's own encodings, small enough to read by eye.
#[cfg(test)]
mod x509 {
    use super::*;
    fn x509_vm() -> vm::Vm {
        let mut v = boot_vm();
        for f in ["src/neant/crypto/der.nt", "src/neant/crypto/x509.nt"] {
            v.eval(&std::fs::read_to_string(f).unwrap()).unwrap_or_else(|e| panic!("{f}: '{}", e.0));
        }
        v
    }
    /// `d "hex"` parses one complete element; anything left over is already an error.
    const D: &str = "d: {derParse unhex x}; ";

    /// Every universal type a certificate uses, decoded from its own encoding.
    #[test]
    fn der_decodes_every_type_a_certificate_uses() {
        let mut v = x509_vm();
        for (src, want) in [
            // INTEGER is two's complement, so the sign is in the top bit of the first octet
            ("derIntVal d \"020100\"", "0"), ("derIntVal d \"02017f\"", "127"),
            ("derIntVal d \"02020080\"", "128"), ("derIntVal d \"0201ff\"", "-1"),
            ("derIntVal d \"020180\"", "-128"),
            ("hex derInt d \"0202ff00\"", "\"ff00\""),          // -256 needs two octets: minimal
            ("derBool d \"0101ff\"", "1b"), ("derBool d \"010100\"", "0b"),
            ("(derNull d \"0500\") ~ 0x", "1b"),
            ("hex derOct d \"0403010203\"", "\"010203\""),
            // the first octet packs two arcs as 40*a+b; { 2 999 3 } is X.690 8.19's own example
            ("derOid d \"06092a864886f70d01010b\"", "\"1.2.840.113549.1.1.11\""),
            ("derOid d \"0603883703\"", "\"2.999.3\""),
            ("derOidSym derOid d \"06092a864886f70d01010b\"", "`sha256WithRsa"),
            ("derOidSym derOid d \"0603883703\"", "`unknown"),
            ("derBitStr d \"030304f0f0\"", "(4;0xf0f0)"),       // 4 unused bits, all of them zero
            ("hex derBits d \"0303001234\"", "\"1234\""),
            ("derStr d \"130461626364\"", "\"abcd\""),          // PrintableString
            ("derStr d \"0c04c3a9c3a8\"", "\"\u{e9}\u{e8}\""),  // UTF8String, decoded as UTF-8
            ("derStr d \"140241e9\"", "\"A\u{e9}\""),           // T61String, decoded as latin-1
            ("derTime d \"170d3236303130313032303330345a\"", "(2026.01.01;02:03:04.000)"),
            ("derTime d \"180f32303236303130313030303030305a\"", "(2026.01.01;00:00:00.000)"),
            // RFC 5280 4.1.2.5.1: a two-digit year is 1950..2049, so 49 and 50 land a century apart
            ("(derTime d \"170d3439303130313030303030305a\")[0]", "2049.01.01"),
            ("(derTime d \"170d3530303130313030303030305a\")[0]", "1950.01.01"),
            // structure: long-form length, the high-tag-number form, and a constructed element's kids
            ("n: d (\"3081c8\"),raze 200#enlist \"00\"; (n[`tag];n[`hlen];n[`len])", "16 3 200"),
            ("h: d \"5f1f0100\"; (h[`cls];h[`tag];h[`cons])", "(`app;31;0b)"),
            ("count derKids d \"3006020101020102\"", "2"),
            ("count derKids d \"3100\"", "0"),
            // the span a node carries is the bytes it came from, not a re-encoding
            ("hex (derKids d \"3006020101020102\")[1][`raw]", "\"020102\""),
        ] {
            assert_eq!(tests::ev(&mut v, &format!("{D}{src}")), want, "source: {src}");
        }
    }

    /// Everything that must not parse. A DER reader that shrugs at any of these hands a verifier an
    /// encoding whose meaning two implementations can disagree about, which is the whole attack.
    #[test]
    fn der_refuses_every_encoding_der_forbids() {
        let mut v = x509_vm();
        for (what, src, msg) in [
            ("indefinite length", "d \"308005000000\"", "indefinite length is not DER"),
            ("trailing bytes", "d \"050000\"", "trailing bytes after the top-level element"),
            ("non-minimal length", "d \"3081050500\"", "non-minimal length"),
            ("length past the end", "d \"30050500\"", "length runs past the end of the input"),
            ("truncated header", "d \"05\"", "input ends inside an element"),
            ("length wider than an int", "d \"3085010000000500\"", "length does not fit an int"),
            ("non-minimal tag", "d \"5f800100\"", "non-minimal tag"),
            ("padded positive INTEGER", "derInt d \"0202007f\"", "non-minimal INTEGER"),
            ("padded negative INTEGER", "derInt d \"0202ffff\"", "non-minimal INTEGER"),
            ("empty INTEGER", "derInt d \"0200\"", "empty INTEGER"),
            ("INTEGER wider than an i64", "derIntVal d \"0209010000000000000000\"",
             "INTEGER too large for an int"),
            ("BOOLEAN that is not 0x00/0xff", "derBool d \"010101\"",
             "BOOLEAN must be 0x00 or 0xff in DER"),
            ("two-octet BOOLEAN", "derBool d \"0102ffff\"", "BOOLEAN must be one octet"),
            ("NULL with content", "derNull d \"050100\"", "NULL with content"),
            ("padded OID subidentifier", "derOid d \"06032a8001\"",
             "non-minimal OBJECT IDENTIFIER subidentifier"),
            ("OID cut mid-subidentifier", "derOid d \"06022a86\"",
             "OBJECT IDENTIFIER ends inside a subidentifier"),
            ("empty OID", "derOid d \"0600\"", "empty OBJECT IDENTIFIER"),
            ("BIT STRING with set unused bits", "derBitStr d \"030304f0f1\"",
             "BIT STRING trailing bits are not zero"),
            ("BIT STRING claiming 8 unused", "derBitStr d \"03020800\"",
             "BIT STRING unused-bit count out of range"),
            ("empty BIT STRING with unused bits", "derBitStr d \"030101\"",
             "BIT STRING has no octets but claims unused bits"),
            ("partial-octet key", "derBits d \"030304f0f0\"",
             "BIT STRING is not a whole number of octets"),
            ("PrintableString with @", "derStr d \"130140\"",
             "PrintableString has a character outside its set"),
            ("IA5String above ASCII", "derStr d \"160180\"", "IA5String is not ASCII"),
            ("constructed string", "derStr d \"3300\"", "a DER string must be primitive"),
            ("UTCTime without seconds", "derTime d \"170b3236303130313030303030\"",
             "UTCTime must be 13 characters"),
            ("UTCTime without Z", "derTime d \"170d323630313031303030303030ff\"",
             "UTCTime must end in Z"),
            ("month 13", "derTime d \"170d3236313330313030303030305a\"",
             "month out of range in UTCTime"),
            ("30 February", "derTime d \"170d3236303233303030303030305a\"",
             "day out of range in UTCTime"),
            ("second 60", "derTime d \"170d3236303130313030303036305a\"",
             "second out of range in UTCTime"),
            ("primitive SEQUENCE", "derWant[d \"1000\";16;1b]", "SEQUENCE must be constructed"),
            ("kids of a primitive", "derKids d \"0500\"",
             "expected a constructed element, got a primitive one"),
        ] {
            assert_eq!(tests::ev(&mut v, &format!("{D}{src}")), format!("'der: {msg}"), "{what}");
        }
    }

    /// The self-signed RSA fixture, field by field. Its key is regenerated by the command in its
    /// header, so nothing here depends on the modulus' value — only on its size and on the fields
    /// the command itself fixes.
    #[test]
    fn self_signed_rsa_matches_openssl() {
        let mut v = x509_vm();
        v.eval("c: x509Parse (pemLoad \"tests/data/selfsigned-rsa.pem\")[0]").unwrap();
        for (src, want) in [
            ("c`ver", "3"),
            ("hex c`serial", "\"0102030405060708\""),            // openssl: serial=0102030405060708
            ("c`sigAlg", "`rsaPkcs1Sha256"),                     // Signature Algorithm: sha256WithRSA
            ("c`sigAlgOid", "\"1.2.840.113549.1.1.11\""),
            ("c`sigParams", "()!()"),                            // PKCS#1 v1.5 has no parameters
            // openssl x509 -noout -subject -issuer -nameopt RFC2253, verbatim
            ("c`subject", "\"CN=selfsigned.neant.test,OU=crypto,O=neant,L=San Francisco,\
ST=California,C=US\""),
            ("(c`issuer) ~ c`subject", "1b"),
            ("(c`issuerCanon) ~ c`subjectCanon", "1b"),            // self-signed: it is its own issuer
            ("c`subjectCanon", "\"2.5.4.6=us,2.5.4.8=california,2.5.4.7=san francisco,\
2.5.4.10=neant,2.5.4.11=crypto,2.5.4.3=selfsigned.neant.test\""),
            ("c`notBefore", "(2026.01.01;00:00:00.000)"),        // Not Before: Jan  1 00:00:00 2026
            ("c`notAfter", "(2036.01.01;00:00:00.000)"),         // Not After : Jan  1 00:00:00 2036
            ("(c`spki)`alg", "`rsa"),
            ("count (c`spki)`n", "256"),                         // Public-Key: (2048 bit)
            ("hex (c`spki)`e", "\"010001\""),                    // Exponent: 65537 (0x10001)
            ("0=`int$((c`spki)`n)[0]", "0b"),                    // the sign octet is stripped
            ("c`san", "(\"selfsigned.neant.test\";\"alt.neant.test\")"),   // the IP: entry is skipped
            ("c`eku", "(\"1.3.6.1.5.5.7.3.1\";\"1.3.6.1.5.5.7.3.2\")"),    // serverAuth, clientAuth
            ("(c`isCa; c`pathLen)", "(1b;2)"),                   // CA:TRUE, pathlen:2
            ("c`keyUsage", "`digitalSignature`keyCertSign`cRLSign"),
            ("c`critUnknown", "()"),                             // both critical ones are modelled
            ("count c`sig", "256"),
            // `tbs is the span, not a re-encoding: it is exactly the bytes at offset 4 of the file
            ("der: (pemLoad \"tests/data/selfsigned-rsa.pem\")[0]; (c`tbs) ~ der[4+til count c`tbs]",
             "1b"),
        ] {
            assert_eq!(tests::ev(&mut v, src), want, "source: {src}");
        }
    }

    /// The EC and RSASSA-PSS fixtures: the two spki shapes and the two signature-algorithm shapes
    /// the RSA fixture cannot reach. PSS names its hash in the parameters, not in the OID.
    #[test]
    fn ec_and_pss_certificates_match_openssl() {
        let mut v = x509_vm();
        v.eval("e: x509Parse (pemLoad \"tests/data/selfsigned-ec.pem\")[0]").unwrap();
        v.eval("p: x509Parse (pemLoad \"tests/data/selfsigned-pss.pem\")[0]").unwrap();
        for (src, want) in [
            ("`int$(e`serial)[0]", "42"),                             // openssl: serial=2A
            ("e`sigAlg", "`ecdsaSha256"),                        // ecdsa-with-SHA256
            ("e`sigAlgOid", "\"1.2.840.10045.4.3.2\""),
            ("e`subject", "\"CN=ec.neant.test,O=neant,C=US\""),
            ("(e`spki)`alg", "`ec"),
            ("(e`spki)`curve", "`p256"),                         // NIST CURVE: P-256
            ("(e`spki)`oid", "\"1.2.840.10045.3.1.7\""),         // ASN1 OID: prime256v1
            ("count (e`spki)`point", "65"),                      // 0x04 and two 32-octet coordinates
            ("e`san", "(\"ec.neant.test\")"),
            // CA:FALSE is the DEFAULT, so DER encodes BasicConstraints as an empty SEQUENCE
            ("(e`isCa; e`pathLen)", "(0b;0N)"),
            ("e`keyUsage", ",`digitalSignature"),
            ("`int$(p`serial)[0]", "123"),                            // openssl: serial=7B
            ("p`sigAlg", "`rsaPssSha256"),                       // Signature Algorithm: rsassaPss
            ("p`sigAlgOid", "\"1.2.840.113549.1.1.10\""),
            // Hash Algorithm: sha256 / Mask Algorithm: mgf1 with sha256 / Salt Length: 0x20
            ("p`sigParams", "`hash`mgfHash`saltLen!(`sha256;`sha256;32)"),
            ("(p`spki)`alg", "`rsa"),
            ("p`keyUsage", "()"),                                // no keyUsage extension at all
        ] {
            assert_eq!(tests::ev(&mut v, src), want, "source: {src}");
        }
    }

    /// A chain exactly as a server sends one. The point of `issuerCanon`/`subjectCanon` is here:
    /// chain building matches one certificate's issuer against another's subject, and it must be a
    /// comparison on a canonical form, not on the printable text.
    #[test]
    fn real_chain_parses_and_links() {
        let mut v = x509_vm();
        v.eval("cs: x509Parse each pemLoad \"tests/data/chain-google.pem\"").unwrap();
        for (src, want) in [
            ("count cs", "3"),                                   // leaf, intermediate, root
            ("(cs[0])`subject", "\"CN=www.google.com\""),
            ("(cs[0])`issuer", "\"CN=WE2,O=Google Trust Services,C=US\""),
            ("hex (cs[0])`serial", "\"465ba8f43add48190abbcabc0878c127\""),
            ("(cs[0])`notBefore", "(2026.09.04;08:06:59.000)"),  // Not Before: Sep 4 08:06:59 2026
            ("(cs[0])`notAfter", "(2026.11.27;08:06:58.000)"),
            ("(cs[0])`sigAlg", "`ecdsaSha256"),
            ("((cs[0])`spki)`curve", "`p256"),
            ("(cs[0])`san", "(\"www.google.com\")"),
            ("((cs[0])`isCa; (cs[0])`pathLen)", "(0b;0N)"),
            ("(cs[1])`subject", "\"CN=WE2,O=Google Trust Services,C=US\""),
            ("(cs[1])`sigAlg", "`ecdsaSha384"),
            ("((cs[1])`isCa; (cs[1])`pathLen)", "(1b;0)"),       // CA:TRUE, pathlen:0
            ("(cs[2])`subject", "\"CN=GTS Root R4,O=Google Trust Services LLC,C=US\""),
            ("(cs[2])`issuer", "\"CN=GlobalSign Root CA,OU=Root CA,O=GlobalSign nv-sa,C=BE\""),
            ("(cs[2])`sigAlg", "`rsaPkcs1Sha256"),               // an EC key cross-signed by RSA
            ("((cs[2])`spki)`curve", "`p384"),
            ("((cs[2])`isCa; (cs[2])`pathLen)", "(1b;0N)"),      // CA:TRUE with no pathlen
            ("{x`critUnknown} each cs", "(();();())"),           // nothing critical is unmodelled
            ("(cs[0])[`issuerCanon] ~ (cs[1])`subjectCanon", "1b"),
            ("(cs[1])[`issuerCanon] ~ (cs[2])`subjectCanon", "1b"),
            ("(cs[0])[`issuerCanon] ~ (cs[2])`subjectCanon", "0b"),
            // the hashes a verifier will actually sign-check, over the spans the parser kept:
            //   python3 -c 'import base64,hashlib,re,sys; ...' -- or equivalently
            //   openssl asn1parse -in <one cert> -strparse 4 -noout -out - | sha256sum
            ("hex sha256 (cs[0])`tbs",
             "\"68d4a9fb5c3da2814f42d61dd2558273479b5f75fd8ae13d7f9e715c3eae7306\""),
            ("hex sha256 (cs[1])`tbs",
             "\"c3210337a3f77559f6371588cdc4894e3d084bb79f4c283f9320b7bd8cf16474\""),
            ("hex sha256 (cs[2])`tbs",
             "\"6e62a0efd60a04d93f2f864b54442717a666c038c8c70546af6319e4f46a2f73\""),
        ] {
            assert_eq!(tests::ev(&mut v, src), want, "source: {src}");
        }
    }

    /// RFC 5280 4.2: a critical extension a verifier does not understand means the certificate must
    /// be rejected. Dropping it silently is a way a verifier gets fooled, so it is reported instead.
    #[test]
    fn a_critical_extension_nobody_models_is_reported() {
        let mut v = x509_vm();
        v.eval("c: x509Parse (pemLoad \"tests/data/critical-unknown-ext.pem\")[0]").unwrap();
        assert_eq!(tests::ev(&mut v, "c`critUnknown"), "(\"1.3.6.1.4.1.99999.1\")");
        assert_eq!(tests::ev(&mut v, "c`subject"), "\"CN=critical.neant.test\"");
        assert_eq!(tests::ev(&mut v, "hex c`serial"), "\"1234\"");
    }

    /// tests/data/malformed.pem, block by block, in the order tests/data/malformed.py writes them.
    /// The last one is the interesting one: two AlgorithmIdentifiers of the same length that say
    /// different things, which only RFC 5280 4.1.1.2's equality check catches.
    #[test]
    fn malformed_certificates_signal() {
        let mut v = x509_vm();
        v.eval("ds: pemLoad \"tests/data/malformed.pem\"").unwrap();
        assert_eq!(tests::ev(&mut v, "count ds"), "5");
        for (i, msg) in [
            (0, "'der: length runs past the end of the input"),
            (1, "'der: non-minimal length"),
            (2, "'der: indefinite length is not DER"),
            (3, "'der: trailing bytes after the top-level element"),
            (4, "'x509: signatureAlgorithm does not match the one inside tbsCertificate"),
        ] {
            assert_eq!(tests::ev(&mut v, &format!("x509Parse ds[{i}]")), msg, "block {i}");
        }
    }

    /// pemDecode ignores everything outside the BEGIN/END lines — which is what lets an
    /// `openssl s_client -showcerts` capture be handed over unedited — and refuses a broken wrapper.
    #[test]
    fn pem_decoding_is_strict_about_its_wrapper() {
        let mut v = x509_vm();
        let one = "-----BEGIN CERTIFICATE-----\\nBQA=\\n-----END CERTIFICATE-----";
        for (src, want) in [
            (format!("hex (pemDecode \"junk\\n{one}\\ntrailing junk\")[0]"), "\"0500\"".to_string()),
            (format!("count pemDecode \"{one}\\n{one}\""), "2".to_string()),
            ("count pemDecode \"nothing here\"".to_string(), "0".to_string()),
            (format!("pemDecode \"{one}\\n-----BEGIN CERTIFICATE-----\""),
             "'pem: a BEGIN CERTIFICATE with no END".to_string()),
            ("pemDecode \"-----END CERTIFICATE-----\"".to_string(),
             "'pem: an END CERTIFICATE with no BEGIN".to_string()),
            (format!("count pemLoad \"tests/data/chain-google.pem\""), "3".to_string()),
        ] {
            assert_eq!(tests::ev(&mut v, &src), want, "source: {src}");
        }
    }
}

#[cfg(test)]
mod rsa {
    use super::*;
    fn rsa_vm() -> vm::Vm {
        let mut v = boot_vm();
        for f in ["src/neant/crypto/bignum.nt", "src/neant/crypto/sha512.nt", "src/neant/crypto/rsa.nt"] {
            v.eval(&std::fs::read_to_string(f).unwrap()).unwrap_or_else(|e| panic!("{f}: '{}", e.0));
        }
        v
    }

    // openssl genrsa -out rsa2048.pem 2048            (OpenSSL 3.6.2)
    // openssl rsa -in rsa2048.pem -noout -modulus     — the exponent is the default 65537
    const N: &str = "d7252f153f75e2036073b54aa1d31ec5846375cefd29a6c7a33d58c1a977caac6e0594ac0962e09c7e59d2f3ec881227436cd323a4ee5de03d15041770b66fe399eee9eab885cbf7be0e90a13112780d6fe79d4dbb2a9f842d3eda4d2e559cbc163b4e71024bc3b62da4dc57ca3d3e34a4ee2f47bf37752fb397a1178b0fdba151fd1c9f311ec9bc7585a95078c3ed249883b4f5247d17cea4aa86398ff94aea8bc595dcc5ed637e9bcf9ad7a9f08b4d672507014f816da600247ea832e78522939654f56a11667dd17839264bd2fc9ec78da4b4b7c2d0d2fd3f2c3df5fa37de3d23365644ae2cc0bbf1ee7e55c161f7dfba2d3b9bb69e4db1c1533f2c6ceb11";
    // printf 'neant rsa test vector\n' > msg.txt && openssl dgst -sha256 msg.txt
    const DIGEST: &str = "d4663dfa5e7a0c65e3c005537874a194dbd6735b382cc18a6c297908f1faef9d";
    // openssl dgst -sha256 -sign rsa2048.pem -out sig.bin msg.txt
    const SIG_PKCS1: &str = "1078ecb416e3904023443b1dda9a5bc3bb1c01318d0db3878667893c6f1a6b1cdef9386255ebfff6e6884d03ab4475ffd41697589f10ee2f71828739cfce4e86b81ceba2e9602831ad13afa873cc5854491f1fb6a13290344dc7e35db547fbf4f2f8cb4b663396617d7355c7635acac2757f6511c572215fdd0b2983d8816fa34a9257247d158e80c5d2155dee8a1eb3085db19e17a82adf68ceb81c09ef7ff1143e64c2cf1b0b6d6f7cfcd81b6d64f59ba05211439f0b24313b4919b57f57a26e850b865bd45a4271152eb1a58cee78105c477665dff8093bda9152d3bde07a52aae9abf47c711ae87999e6e7fd0189328aebdf4ad3e80515447f6d65b80657";
    // openssl dgst -sha256 -sigopt rsa_padding_mode:pss -sigopt rsa_pss_saltlen:<32|20|0> \
    //   -sign rsa2048.pem -out sig.bin msg.txt
    const SIG_PSS32: &str = "6000dc8a4802b8ad752abbbbae0fdc0f4944f011e6e1c090389ba7581df83c655f204f0a1355c31d0a4fcf16ea4123016f8fabe1e5fc82b696818d42fdae7bb016db84fb3097ddac8b5ad2de0cd0f569d239a30a026ab0f52fcec14ffcd3e947b7d5ecb4d6367a81a7bedcf6e4b02be9b38b7d89fcb6b7ae912176449a166fba528460ceec28b8a62e6a53c9e66b2a64be39d820e46c5f0797402820b3618a67e1c73f208e7b7082e6479e5cf80e9839ec62f1f637ba3c128fd590b5f366c85b8ea59df77ccee60cf4e78a36f8cd4a2c89afad2c34c05e5d0e88f092325b01b7032ada80e99fcd4caeaf8ae7ba0d8e98797daadbac50580fb6fcac06d9ac6f61";
    const SIG_PSS20: &str = "974980df531a3e259146454c3c7f4c580faaba6c58b1cc5ae4d219a9b27af2ef5f222490b756c51d91f4f41c7266544e91f4a23f3c457d1c348196d981c247188acae32a28575c7c60ba4f37b9005013e81f8c902bc138f0822e1ed6c51202452df96d65e98cb783521c45fe5f6371748762eb4563c0096246e1626340bae4e7c5cd60bdd250cb7a870e351250a78314f6c4bd1b6b2dc861269171e521bd953ef6766970c0b497a4afd6a15c6db510bda9aa6aa43dcf68f493aa5005e46f6786ae5460dac8d5748752f6c426e6bc0f40fc889dee7d33417e9ddeb3cde290d3c5814430c4df8ac6390e58919bb5492a251a7ebbedaa63e33a6e7aa1fd3dbf7d44";
    const SIG_PSS0: &str = "1483891a33d1d6255da5fc475e8a5ce5cda15294d30bfa1f41e6dee71340ed14d6d3411389a5610ea7e621f964a9a447c8136766edb283069c968e44cb90cc16f587acfa3f884fc441a0823d8cb2849981c5e9b3064c5d89ca63b25be828c749099b0588999a97f9258b6a40f53f189615add933bc457dcd691211824ee57378986c43b4231b62bb9bde54ed0b075675ae4dd9765cad577a7f9c7ca509aa7b697e959b95cb82699da10b746035fe1cfd7a9d799469b1e7435cd809af621635b3ed9c6809626e45cee0e49b06a2e6b08bee884748ade5a84618d51479d2736fd3c2bb560061dede5f5f6d1cfcc816cbb593d356a5475919bf54b2f112a2cd5f9a";
    // The forgeries. openssl will not produce a bad block, so these are `pow(EM, d, n)` in python
    // over the d that `openssl rsa -in rsa2048.pem -noout -text` prints, with EM built by hand.
    // BLEICH_EM is 0x00 0x01 <eight 0xFF> 0x00 DigestInfo <0xAA filler>: the right digest, the right
    // DigestInfo, and 245 bytes of garbage where the padding should be — Bleichenbacher '06.
    const BLEICH_SIG: &str = "7ca59fd84f87ff5a2cebb684aab0dda8f5df6aaa687fcfadf670f97eb0d8614ff0f96a2f8c565872101ea854192ce9a30ae25f94a61d1186acbbc4eea7647df2362ad6a337dabf266d4aefa354e157916b4e64ee0d64a4d5bb231acb362b22659bd1c23789189029c69c6b0f3c2f7eda9060df66de4e381fc029e36b16690513b9816cf9966e1ed6cec93d31069a9b9123eb4cee4a906eb55c383f79eff42b75def548304730fd5f19c035c39b46b7192ac469150ca6af72f92ab7d754812dab10224be96dc7ab003926444235dc30df82f6980173e5008912595c7457989e7bcd965b6b636f63a7dd942e072dcd60c0e3302ea9cbcc93891ad60470d5dd02a1";
    const BLEICH_EM: &str = "0001ffffffffffffffff003031300d060960864801650304020105000420d4663dfa5e7a0c65e3c005537874a194dbd6735b382cc18a6c297908f1faef9daaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    // A hand-built PSS block (salt 00 01 .. 1f) and three corruptions of it, all signed the same way.
    const PSS_HAND: &str = "64cc48620dc22058678213f5d3fd19a1b665dd07bee9b0b1ce00880e5d682c966651cfd1d5f194b16def32a29bf8fff8e3c9629afe5df479dec282c50be14c4338ea206d01b4c7989ccbbc55f338d781083fd45ccf20b3d3a3dae198d59f65c7d817fe025ae0d0e8b096e151aef55d54bfe08cb76345856ad80ed0a1b19e12923740629e4e6eb1042e82bce30bda1cc0650ec300d5c4d1b5f57b5180593716fedd2b5ab01156c19f1150a29504096128d7a34e97035defd34f2e793092d40458d84d0e9a7b5adc455f98066f70bf43138be59dd0156ac9773a9ef3438bb1a626ebf6152cef3ce4104cdb0e0faf37eca6d7e6c91f3719275829e10f7dc72fcff7";
    const PSS_TRAIL: &str = "cee0346c14fee0c6e038d5d47580d13e594bfd16b747063fc1e7bda198858fe34616e3194bea699e5ffcab055ae6187f071dc16809a51654b587d75c23d385ca973a61f96844e288a1a4dfdd5f25088dc0e2c7213af0f72f99f84b11cd875c467398951d7fca58d0830588ecf2acf3e2a2e0c40202493c95c7506aaa6f3ea71eba6f326d1d80d004bb65fcbeb409dbbb7cd1e019fc3dbe643e8fc3693169d1bfe02f12d6e6472aee72b4b09fac4156b0942346db47b40a4c73089b51722339c19fc8b852181e79ef26c05214cdbb0d6b035e624142f9557b146311fd4192aa0de803537ecf9b9d5bc685c174be90eaef961c17924a0e11c4eef45233184b0854";      // 0xbc trailer turned into 0xbb
    const PSS_TOPBIT: &str = "8b2d4a40ad4ad70eedf69bc532faad24a65e4200956da0ec80e536267ad2bc7d553aa10efbb3b679723308b392b7cfd3e16c0fc5667603d6d88c6ef8645b302e154a0e736fe50cff7c8c31af584fcd55af6582ffdec8eadf314bb64636004f651f18974458b2cf253410fec2ae01b857fde59a43311b145743f4bc2e1c96ca3609f1b1be55fc49b5246dddfca2b784a426a8c48e751ffba62f9b126fed067da10c1dd61aac356629de1e06f3b103c36d992ca31415da5bf7da7566711fd2b3b40a46e6127d6aea86a6014cc280ba8e0e49d383a15c8abf7c5fb6a937bb77ca52d06e6c0a85746e3a265fad3523c07d0c3dc37bd1fefea8021d6fb027ea0bd81d";    // top bit of maskedDB set, which emBits forbids
    const PSS_PS: &str = "5103acac7b014c32a8f7fd5a747bffb02823dbe81974030d0628757c1f41e95a6435209855e15d1f1484f734ab08a94412305f6382baa7faafd96de4c2529ef3f3b2e89fd8906b2e5a9f7cab8072ec6789b7d55343919dd640a83424c4f392c10fd1364c2f22d7e4ecebb4272f3cc97a13e65fbe37a1f6078392cdbd374e905c702cd5eceb71338c37a2085577f2088a5127923203460fe8a09b60057e5c5a67ec21a3ec78d288da1348e57ee3228802b7e1d669f87d0bddb7d609d379cff8c1977e7cd9e4724ec58ad4b9c66d271634dc21bc493dd3bbfcf4314fac85ebb42545cd823f0b1cc9eabb372d9f93c67f1f873d8142236dd201904cbaed67ee62ae";            // a 0x07 in DB's zero padding, before the 0x01
    // With e = 1 the "signature" is the encoded message itself — this is a correct PKCS#1 block.
    const E1_SIG: &str = "0001ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff003031300d060960864801650304020105000420d4663dfa5e7a0c65e3c005537874a194dbd6735b382cc18a6c297908f1faef9d";

    /// n, e, the digest and one signature, bound in the VM so a test reads as one line.
    fn key(sig: &str) -> String {
        format!("n: unhex \"{N}\"; e: 0x010001; dg: unhex \"{DIGEST}\"; s: unhex \"{sig}\"; ")
    }

    /// The limb layer under everything else: 26-bit limbs in and out of big-endian bytes, and the
    /// four operations bnModExp is built from. python3 -c "print(hex(<expr>))" for each.
    #[test]
    fn bignum_arithmetic_matches_python() {
        let mut v = rsa_vm();
        for (src, want) in [
            // the representation itself: 2^26 is one limb over, and zero has no bytes at all
            ("bnFromBytes 0x04000000", "0 1"),
            ("hex bnBytes bnFromBytes 0x000000deadbeef", "\"deadbeef\""),   // leading zeros do not survive
            ("count bnBytes bnFromBytes 0x00", "0"),
            ("hex bnBytesN[bnFromBytes 0xff; 4]", "\"000000ff\""),
            ("bnBits bnFromBytes 0x0100", "9"), ("bnBits bnFromBytes 0x", "0"),
            ("(bnCmp[bnFromBytes 0x02; bnFromBytes 0x0100]; bnCmp[bnFromBytes 0x02; bnFromBytes 0x02])", "-1 0"),
            // hex(0xffffffffffffffff * 0xffffffffffffffff)
            ("hex bnBytes bnMul[bnFromBytes 0xffffffffffffffff; bnFromBytes 0xffffffffffffffff]",
             "\"fffffffffffffffe0000000000000001\""),
            // hex(0x100000000000000000000 - 1), a borrow through every limb
            ("hex bnBytes bnSub[bnFromBytes 0x0100000000000000000000; bnFromBytes 0x01]",
             "\"ffffffffffffffffffff\""),
            ("hex bnBytes bnAdd[bnFromBytes 0xffffffffffffffffffff; bnFromBytes 0x01]",
             "\"0100000000000000000000\""),
            ("hex bnBytes bnShl[bnFromBytes 0x01; 100]", "\"10000000000000000000000000\""),
            ("hex bnBytes bnShr[bnFromBytes 0x10000000000000000000000000; 90]", "\"0400\""),
            // divmod(0xdeadbeefcafebabe1337, 0x0123456789abcdef)
            ("d: bnDivMod[bnFromBytes 0xdeadbeefcafebabe1337; bnFromBytes 0x0123456789abcdef]; \
              (hex bnBytes d[0]; hex bnBytes d[1])", "(\"c3b6b4\";\"ed8473ec7c5d2b\")"),
            ("hex bnBytes bnMod[bnFromBytes 0xdeadbeefcafebabe1337; bnFromBytes 0x0123456789abcdef]",
             "\"ed8473ec7c5d2b\""),
            ("bnBytes bnSub[bnFromBytes 0x01; bnFromBytes 0x02]", "'bn: subtraction would go negative"),
        ] {
            assert_eq!(tests::ev(&mut v, src), want, "source: {src}");
        }
    }

    /// bnModExp against python3 -c "print(pow(b,e,m))" — three small ones that exercise the odd
    /// corners, then the RSA-2048 exponentiation an actual verification performs.
    #[test]
    fn bnmodexp_matches_python_pow() {
        let mut v = rsa_vm();
        for (src, want) in [
            ("`int$bnModExp[0x05; 0x03; 0x0d]", ",8"),                              // pow(5,3,13) = 8
            ("hex bnModExp[0x02; 0x0a; unhex \"3b9aca07\"]", "\"0400\""),          // pow(2,10,1000000007)
            ("hex bnModExp[unhex \"deadbeef\"; 0x010001; unhex \"fffffffb\"]",
             "\"8e338be9\""),                                                     // pow(0xdeadbeef,65537,0xfffffffb)
            ("count bnModExp[0x; 0x05; 0x61]", "0"),                              // pow(0,5,97) = 0
            ("`int$bnModExp[0x03; 0x; 0x07]", ",1"),                                // an empty exponent is zero
            ("bnModExp[0x05; 0x03; 0x0c]", "'bn: even modulus, Montgomery reduction needs an odd one"),
            ("bnModExp[0x05; 0x03; 0x]", "'bn: modulus is zero"),
        ] {
            assert_eq!(tests::ev(&mut v, src), want, "source: {src}");
        }
        // python3 -c "print(hex(pow(<SIG_PKCS1>, 65537, <N>)))" — the recovered PKCS#1 block, whose
        // leading 0x00 is not in the minimal byte form bnModExp returns.
        let src = format!("{}hex bnModExp[s; e; n]", key(SIG_PKCS1));
        assert_eq!(tests::ev(&mut v, &src), "\"01ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff003031300d060960864801650304020105000420d4663dfa5e7a0c65e3c005537874a194dbd6735b382cc18a6c297908f1faef9d\"");
    }

    /// The signatures openssl made over msg.txt, with the salt length recovered from the encoding
    /// rather than told to the verifier: 32, 20 and 0 all have to work.
    #[test]
    fn openssl_signatures_verify() {
        let mut v = rsa_vm();
        for (what, sig, call) in [
            ("pkcs1-v1_5", SIG_PKCS1, "rsaVerifyPkcs1[n;e;`sha256;dg;s]"),
            ("pss salt 32", SIG_PSS32, "rsaVerifyPss[n;e;`sha256;dg;s]"),
            ("pss salt 20", SIG_PSS20, "rsaVerifyPss[n;e;`sha256;dg;s]"),
            ("pss salt 0", SIG_PSS0, "rsaVerifyPss[n;e;`sha256;dg;s]"),
            ("pss hand-built, salt 32", PSS_HAND, "rsaVerifyPss[n;e;`sha256;dg;s]"),
        ] {
            let src = format!("{}{call}", key(sig));
            assert_eq!(tests::ev(&mut v, &src), "1b", "did not verify: {what}");
        }
    }

    /// A verifier's whole job. One flipped bit anywhere, a signature that is not exactly k bytes, a
    /// scheme confusion, an unknown hash, a digest of the wrong length, and e = 1.
    #[test]
    fn tampering_and_malformed_signatures_are_rejected() {
        let mut v = rsa_vm();
        let p1 = key(SIG_PKCS1);
        let ps = key(SIG_PSS32);
        for (what, src) in [
            ("flipped last bit of the pkcs1 signature", format!("{p1}rsaVerifyPkcs1[n;e;`sha256;dg;(255#s),bnot s[255]]")),
            ("flipped first bit of the pkcs1 signature", format!("{p1}rsaVerifyPkcs1[n;e;`sha256;dg;(bnot s[0]),1_s]")),
            ("flipped bit in the digest", format!("{p1}rsaVerifyPkcs1[n;e;`sha256;(31#dg),bnot dg[31];s]")),
            ("signature one byte short", format!("{p1}rsaVerifyPkcs1[n;e;`sha256;dg;255#s]")),
            ("signature one byte long", format!("{p1}rsaVerifyPkcs1[n;e;`sha256;dg;s,0x00]")),
            ("e = 1 over a correct pkcs1 block", format!("n: unhex \"{N}\"; dg: unhex \"{DIGEST}\"; rsaVerifyPkcs1[n;0x01;`sha256;dg;unhex \"{E1_SIG}\"]")),
            ("unknown hash symbol", format!("{p1}rsaVerifyPkcs1[n;e;`sha1;dg;s]")),
            ("digest of the wrong length", format!("{p1}rsaVerifyPkcs1[n;e;`sha256;31#dg;s]")),
            ("a pss signature offered to the pkcs1 verifier", format!("{ps}rsaVerifyPkcs1[n;e;`sha256;dg;s]")),
            ("flipped last bit of the pss signature", format!("{ps}rsaVerifyPss[n;e;`sha256;dg;(255#s),bnot s[255]]")),
            ("flipped bit in the digest, pss", format!("{ps}rsaVerifyPss[n;e;`sha256;(31#dg),bnot dg[31];s]")),
            ("pss signature one byte short", format!("{ps}rsaVerifyPss[n;e;`sha256;dg;255#s]")),
            ("a pkcs1 signature offered to the pss verifier", format!("{p1}rsaVerifyPss[n;e;`sha256;dg;s]")),
            ("an even modulus: 0b, not the signal bnModExp would raise", format!("{p1}rsaVerifyPkcs1[(255#n),0x00;e;`sha256;dg;s]")),
            // 32776 bits is 1261 limbs, past the 1023 bnMul insists on — rsaRecover's width cap has
            // to turn that into 0b before bnMul gets the chance to signal, or a certificate could
            // make the verifier raise instead of answering.
            ("a 32776-bit modulus with a signature to match", format!("{p1}rsaVerifyPkcs1[(4096#0xff),0x01;e;`sha256;dg;4097#0x02]")),
        ] {
            assert_eq!(tests::ev(&mut v, &src), "0b", "accepted a forgery: {what}");
        }
    }

    /// Bleichenbacher '06: the block really does carry the right DigestInfo for the right digest,
    /// and a verifier that scans for it instead of checking the whole encoding accepts it. The first
    /// assertion proves the vector is the attack and not just noise — the recovered block is
    /// byte-for-byte the crafted one — and the second is the check that has to say no.
    #[test]
    fn bleichenbacher_forged_padding_is_rejected() {
        let mut v = rsa_vm();
        let k = key(BLEICH_SIG);
        // the leading 0x00 of the block is not in bnModExp's minimal byte form
        assert_eq!(tests::ev(&mut v, &format!("{k}(bnModExp[s;e;n]) ~ 1_unhex \"{BLEICH_EM}\"")), "1b");
        assert_eq!(tests::ev(&mut v, &format!("{k}b: unhex \"{BLEICH_EM}\"; (b[til 11] ~ 0x0001ffffffffffffffff00; b[11+til 51] ~ (unhex \"3031300d060960864801650304020105000420\"),dg)")), "11b");
        assert_eq!(tests::ev(&mut v, &format!("{k}rsaVerifyPkcs1[n;e;`sha256;dg;s]")), "0b");
    }

    /// The three PSS encoding rules that are easy to skip, each one broken on its own in a block
    /// that is otherwise exactly PSS_HAND — which verifies, so the broken bit is the only change.
    #[test]
    fn pss_encoding_rules_are_enforced() {
        let mut v = rsa_vm();
        for (what, sig) in [
            ("0xbc trailer", PSS_TRAIL),
            ("the leftmost 8*emLen-emBits bits of maskedDB", PSS_TOPBIT),
            ("the zero padding before DB's 0x01 separator", PSS_PS),
        ] {
            let src = format!("{}rsaVerifyPss[n;e;`sha256;dg;s]", key(sig));
            assert_eq!(tests::ev(&mut v, &src), "0b", "ignored {what}");
        }
    }

    /// A bad signature is an answer (0b); a missing key is a bug at the call site, and signals.
    #[test]
    fn empty_key_material_signals() {
        let mut v = rsa_vm();
        let k = key(SIG_PKCS1);
        assert_eq!(tests::ev(&mut v, &format!("{k}rsaVerifyPkcs1[0x;e;`sha256;dg;s]")), "'rsa: modulus is empty");
        assert_eq!(tests::ev(&mut v, &format!("{k}rsaVerifyPss[n;0x;`sha256;dg;s]")), "'rsa: exponent is empty");
    }

    /// e = 65537 is sixteen squarings and two multiplies, so the whole verification is about twenty
    /// 2048-bit Montgomery multiplications plus the one division that makes R^2 mod N. The bound is
    /// loose — the number in the log is the point.
    #[test]
    fn rsa2048_verification_time() {
        let mut v = rsa_vm();
        let src = format!("{}t0: now`time; do[20; rsaVerifyPkcs1[n;e;`sha256;dg;s]]; `int$now[`time]-t0", key(SIG_PKCS1));
        let ms: f64 = tests::ev(&mut v, &src).parse().unwrap();
        eprintln!("RSA-2048 PKCS#1 v1.5 verification: {:.1}ms", ms / 20.0);
        assert!(ms < 4000.0, "an RSA-2048 verification took {}ms", ms / 20.0);
    }
}

/// ECDSA P-256 verification (src/neant/crypto/p256.nt), for the one thing that needs the host: how
/// long one signature takes. Everything else -- the field, the group law against published multiples
/// of the base point, the openssl vector, and every malformed point, scalar and DER encoding that
/// must come back 0b -- is pure neant and lives in tests/p256.nt, which tests::nt_tests runs.
#[cfg(test)]
mod p256 {
    use super::*;
    fn p256_vm() -> vm::Vm {
        let mut v = boot_vm();
        for m in ["bignum", "der", "ec", "p256"] {
            let f = format!("src/neant/crypto/{m}.nt");
            v.eval(&std::fs::read_to_string(&f).unwrap()).unwrap_or_else(|e| panic!("{f}: '{}", e.0));
        }
        v
    }
    /// The vector is tests/p256.nt's, made from the committed tests/data/pki-ec-leaf.key; the same
    /// comment there carries the openssl commands.
    const PUB: &str = "04d80beadaa91bcac982be71619c25106a5d257a6537d2233e689e2c72e6615843873e23bee15467e76c55be95400a63d44cc891a45adbcf74785bf675ebc7da45";
    const DG: &str = "ddf5bf46337c63b4cef6f1dbfac137cb3e0a8a6e00313db0f98d3d8f9872782d";
    const SIG: &str = "3046022100b561094b230ffe88c2f54eeb4dea56d3ddc4c0c0086ad91939a667e88be6373c022100f090ab3512ae71e1c2af3ca720f081e79bb11deb367652e23a8e5fd7e804e8b4";

    /// A verification is one inversion mod n, 256 Jacobian doublings and about 192 mixed additions
    /// over Shamir's trick, and one inversion mod p to come back to affine -- roughly 4900 modular
    /// multiplications, against the twenty an RSA-2048 exponentiation with e = 65537 costs. That
    /// ratio is the whole story and the number in the log is the point; the bound is loose.
    #[test]
    fn p256_verification_time() {
        let mut v = p256_vm();
        let setup = format!("pk: unhex \"{PUB}\"; dg: unhex \"{DG}\"; sg: unhex \"{SIG}\"\n");
        assert_eq!(tests::ev(&mut v, &format!("{setup}ecdsaVerifyP256Der[pk;dg;sg]")), "1b");
        let src = format!("{setup}t0: now`time; do[5; ecdsaVerifyP256Der[pk;dg;sg]]; `int$now[`time]-t0");
        let ms: f64 = tests::ev(&mut v, &src).parse().unwrap();
        eprintln!("ECDSA P-256 verification: {:.1}ms", ms / 5.0);
        assert!(ms < 5000.0, "a P-256 verification took {}ms", ms / 5.0);
    }
}

/// ECDSA P-384 verification (src/neant/crypto/p384.nt) on the curve-generic arithmetic in ec.nt,
/// for the one thing that needs the host: how long one signature takes, and how that compares with
/// P-256 through the same code. Everything else -- the field, the group law, the openssl vector,
/// the constructed R.x >= n case and every malformed input -- is pure neant and lives in
/// tests/p384.nt, which tests::nt_tests runs.
#[cfg(test)]
mod p384 {
    use super::*;
    fn p384_vm() -> vm::Vm {
        let mut v = boot_vm();
        for m in ["bignum", "der", "ec", "p256", "p384"] {
            let f = format!("src/neant/crypto/{m}.nt");
            v.eval(&std::fs::read_to_string(&f).unwrap()).unwrap_or_else(|e| panic!("{f}: '{}", e.0));
        }
        v
    }
    /// The vector is tests/p384.nt's, made from the committed tests/data/pki-p384-leaf.key; the
    /// same comment there carries the openssl commands.
    const PUB: &str = "0401fcb79a963bd5815d8f49cd64138db29b0771d1624f56a8bda75fb8bbf8ed317dc0f8d7f8752b907e69a9f644049dee5456cbfe1060afdbb146c274803cab07d1a83f31f6912ffcab4bb2f45d622a5ae65b2b53cd6467038b022ce205dd3efe";
    const DG: &str = "66101a247503bc96b1bb16792721965500ceba764c4dc1131595933366225ec7099a5d722ccff591439f041453317839";
    const SIG: &str = "30640230392fde917a8a81e7a35e7263d0dbc317177d5e73823535aa2ca845e39a80e6569f996a776786489b5181b2775c779a0402306ebb460609c568f9319f3210b8c6524bb70e2b46887d7a55d29477bfaa0b48f6da00a6d39785423e0b0b1b9ff2953577";
    /// The P-256 vector, run through the same loop in the same VM so the ratio in the log is
    /// measured rather than remembered: the two curves share every line of arithmetic, so what
    /// differs is 384 doublings against 256 and 15-limb multiplications against 10-limb ones.
    const PUB256: &str = "04d80beadaa91bcac982be71619c25106a5d257a6537d2233e689e2c72e6615843873e23bee15467e76c55be95400a63d44cc891a45adbcf74785bf675ebc7da45";
    const DG256: &str = "ddf5bf46337c63b4cef6f1dbfac137cb3e0a8a6e00313db0f98d3d8f9872782d";
    const SIG256: &str = "3046022100b561094b230ffe88c2f54eeb4dea56d3ddc4c0c0086ad91939a667e88be6373c022100f090ab3512ae71e1c2af3ca720f081e79bb11deb367652e23a8e5fd7e804e8b4";

    #[test]
    fn p384_verification_time() {
        let mut v = p384_vm();
        let setup = format!("pk: unhex \"{PUB}\"; dg: unhex \"{DG}\"; sg: unhex \"{SIG}\"\n");
        assert_eq!(tests::ev(&mut v, &format!("{setup}ecdsaVerifyP384Der[pk;dg;sg]")), "1b");
        let src = format!("{setup}t0: now`time; do[5; ecdsaVerifyP384Der[pk;dg;sg]]; `int$now[`time]-t0");
        let ms384: f64 = tests::ev(&mut v, &src).parse().unwrap();
        let setup = format!("pk: unhex \"{PUB256}\"; dg: unhex \"{DG256}\"; sg: unhex \"{SIG256}\"\n");
        assert_eq!(tests::ev(&mut v, &format!("{setup}ecdsaVerifyP256Der[pk;dg;sg]")), "1b");
        let src = format!("{setup}t0: now`time; do[5; ecdsaVerifyP256Der[pk;dg;sg]]; `int$now[`time]-t0");
        let ms256: f64 = tests::ev(&mut v, &src).parse().unwrap();
        eprintln!("ECDSA P-384 verification: {:.1}ms  (P-256 in the same VM: {:.1}ms, ratio {:.2}x)",
                  ms384 / 5.0, ms256 / 5.0, ms384 / ms256);
        assert!(ms384 < 10000.0, "a P-384 verification took {}ms", ms384 / 5.0);
    }
}

/// src/neant/crypto/verify.nt and the TLS 1.3 wiring in tls.nt, for the part of them that needs
/// the host: a real `openssl s_server` to handshake against, and the timings. Everything that is
/// pure neant -- hostname matching, every bad chain and its reason, the Certificate and
/// CertificateVerify messages -- lives in tests/verify.nt, which tests::nt_tests runs.
///
/// The fixture PKI is built by tests/data/pki-gen.sh (RSA) and tests/data/pki-ec-gen.sh (ECDSA),
/// which record the exact `openssl` (3.6.2) commands and pin every date. Nothing here reaches the
/// network: every server is on 127.0.0.1.
#[cfg(test)]
mod verify {
    use super::*;

    fn verify_vm() -> vm::Vm {
        let mut v = boot_vm();
        for m in ["bignum", "sha512", "rsa", "der", "ec", "p256", "p384", "x509", "verify", "tls"] {
            let f = format!("src/neant/crypto/{m}.nt");
            v.eval(&std::fs::read_to_string(&f).unwrap()).unwrap_or_else(|e| panic!("{f}: '{}", e.0));
        }
        // P reads one fixture; RS is a trust store holding only the fixture root; NOW is a fixed
        // instant inside every good fixture's validity window -- the same three tests/verify.nt
        // sets up, so a case can be moved between the two files unchanged.
        for src in ["P: {[f] x509Parse (pemLoad \"tests/data/\",f,\".pem\")[0]}",
                    "RS: x509LoadRoots \"tests/data/pki-root.pem\"",
                    "NOW: (2026.09.16; 12:00:00.000)"] {
            v.eval(src).unwrap_or_else(|e| panic!("{src}: '{}", e.0));
        }
        v
    }
    /// Two fixture subjects, spelled the way x509.nt's RFC 2253 `subject` spells them.
    const LEAF: &str = "CN=leaf.neant.test,O=neant fixtures,C=US";
    const INT: &str = "CN=neant fixture intermediate,O=neant fixtures,C=US";
    const ROOT: &str = "CN=neant fixture root,O=neant fixtures,C=US";

    /// What a verified chain costs: two RSA-2048 signature checks and the rest of RFC 5280 6.1.
    #[test]
    fn chain_verification_time() {
        let mut v = verify_vm();
        let src = "t0: now`time; do[10; x509VerifyChain[(P \"pki-leaf\"; P \"pki-int\"); RS; \
                   \"leaf.neant.test\"; NOW]]; `int$now[`time]-t0";
        let ms: f64 = tests::ev(&mut v, src).parse().unwrap();
        eprintln!("chain verification (leaf + intermediate + root, RSA-2048): {:.1}ms", ms / 10.0);
        assert!(ms < 10000.0, "a two-link chain took {}ms", ms / 10.0);
    }

    /// The trust store over the real system bundle -- how big, how long, and that every root in it
    /// is found by the canonical subject a chain would come looking with.
    #[test]
    fn the_system_trust_store_loads() {
        let path = "/etc/ssl/certs/ca-certificates.crt";
        if !std::path::Path::new(path).exists() {
            eprintln!("no {path} on this machine, skipping");
            return;
        }
        let mut v = verify_vm();
        let ms: f64 = tests::ev(&mut v, "t0: now`time; S: x509SystemRoots[]; `int$now[`time]-t0")
            .parse().unwrap();
        let n: usize = tests::ev(&mut v, "count S`certs").parse().unwrap();
        eprintln!("trust store: {n} roots from {path} in {ms}ms, {} unparseable",
                  tests::ev(&mut v, "S`skipped"));
        assert!(n > 100, "only {n} roots in {path}");
        assert_eq!(tests::ev(&mut v, "all {0<count x509Find[S;x`subjectCanon]} each S`certs"), "1b");
    }

    /// `openssl s_server -rev` on a free port with a fixture certificate: it echoes every line back
    /// reversed, which is the smallest thing that proves data really crosses the record layer after
    /// the handshake. Killed on drop, so a failing assertion still cleans up.
    struct Server(std::process::Child, u16);
    impl Drop for Server { fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); } }
    fn s_server(cert: &str) -> Server { s_server_with(cert, "pki-leaf.key", "pki-int.pem") }
    fn s_server_with(cert: &str, key: &str, chain: &str) -> Server {
        // src/prims.rs has no way to ask a listener its bound port and openssl has none either, so
        // a probe listener picks a free one and is dropped (the same accepted TOCTOU race as the
        // accept-workers test in mod tls).
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let child = std::process::Command::new("openssl")
            .args(["s_server", "-accept", &format!("127.0.0.1:{port}"),
                   "-cert", &format!("tests/data/{cert}.pem"),
                   "-key", &format!("tests/data/{key}"),
                   "-cert_chain", &format!("tests/data/{chain}"), "-tls1_3", "-groups", "x25519",
                   "-ciphersuites", "TLS_CHACHA20_POLY1305_SHA256", "-rev", "-quiet"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
            .spawn().expect("openssl s_server (OpenSSL 3.6.2 makes the fixtures too)");
        // no -naccept, so the server keeps looping and this probe connection costs nothing: a real
        // readiness check instead of a sleep long enough to hope
        for _ in 0..200 {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() { break; }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        Server(child, port)
    }
    /// One tlsConnectOpts against that server with `store` as the trust store, then a send/recv
    /// round trip -- or, when the certificate does not check out, the sentence it was refused with.
    fn talk(v: &mut vm::Vm, port: u16, host: &str, store: &str) -> String {
        v.set("port", value::chars(port.to_string().chars().collect()));
        let src = format!(
            "O: (enlist `roots)!enlist {store}\n\
             h: tlsConnectOpts[\"{host}\"; `int$port; O]\n\
             tlsSend[h; \"hello tls\\n\"]\n\
             r: `char$tlsRecv h\n\
             n: count tlsCert h\n\
             tlsClose h\n\
             (n; r)");
        // a signal from a multi-line source comes back with the call stack under it; the first
        // line is the sentence tlsConnectOpts refused with, which is what these cases are about
        tests::ev(v, &src).split(" at line ").next().unwrap().to_string()
    }

    /// The whole thing: a real TLS 1.3 handshake with a real server, a real chain verified against
    /// a real trust store, and then bytes both ways.
    #[test]
    fn handshake_verifies_and_data_round_trips() {
        let srv = s_server("pki-leaf");
        let mut v = verify_vm();
        // the leaf's SAN carries IP:127.0.0.1, so the address literal is matched against the
        // iPAddress entry and never against a name
        assert_eq!(talk(&mut v, srv.1, "127.0.0.1", "RS"), "(2;\"slt olleh\n\")");
    }
    /// The same thing with an ECDSA certificate, which is the whole point of src/neant/crypto/
    /// p256.nt: the leaf and the root are both P-256 signed with ecdsa-with-SHA256, the server picks
    /// ecdsa_secp256r1_sha256 for its CertificateVerify because the ClientHello now offers it, and
    /// nothing in this handshake goes anywhere near rsa.nt. Before p256.nt this server could not be
    /// talked to at all.
    #[test]
    fn an_ecdsa_handshake_verifies_and_data_round_trips() {
        let srv = s_server_with("pki-ec-leaf", "pki-ec-leaf.key", "pki-ec-root.pem");
        let mut v = verify_vm();
        let store = "x509LoadRoots \"tests/data/pki-ec-root.pem\"";
        assert_eq!(talk(&mut v, srv.1, "127.0.0.1", store), "(2;\"slt olleh\n\")");
    }
    /// The same again on P-384 with SHA-384, which is what the public web's intermediates are
    /// signed with and what this build could not check at all before sha512.nt and p384.nt: the
    /// leaf and the root are both P-384 signed ecdsa-with-SHA384, and the server picks
    /// ecdsa_secp384r1_sha384 (0x0503) for its CertificateVerify because the ClientHello now
    /// offers it. Nothing in this handshake goes near rsa.nt or p256.nt.
    #[test]
    fn a_p384_handshake_verifies_and_data_round_trips() {
        let srv = s_server_with("pki-p384-leaf", "pki-p384-leaf.key", "pki-p384-root.pem");
        let mut v = verify_vm();
        let store = "x509LoadRoots \"tests/data/pki-p384-root.pem\"";
        assert_eq!(talk(&mut v, srv.1, "127.0.0.1", store), "(2;\"slt olleh\n\")");
    }
    /// And an RSA chain signed sha384WithRSAEncryption, which is the other half of what was
    /// missing: the same fixture intermediate and root as the SHA-256 handshake above, and only
    /// the leaf's signature algorithm changed.
    #[test]
    fn a_sha384_rsa_handshake_verifies_and_data_round_trips() {
        let srv = s_server_with("pki-sha384-leaf", "pki-leaf.key", "pki-int.pem");
        let mut v = verify_vm();
        assert_eq!(talk(&mut v, srv.1, "127.0.0.1", "RS"), "(2;\"slt olleh\n\")");
    }
    /// What a P-384 chain costs against the P-256 one below it: one ECDSA-SHA384 verification
    /// rather than one ECDSA-SHA256, through the same code in ec.nt.
    #[test]
    fn p384_chain_verification_time() {
        let mut v = verify_vm();
        let src = "t0: now`time; do[5; x509VerifyChain[enlist P \"pki-p384-leaf\"; \
                   x509LoadRoots \"tests/data/pki-p384-root.pem\"; \"leaf.neant.test\"; NOW]]; \
                   `int$now[`time]-t0";
        let ms: f64 = tests::ev(&mut v, src).parse().unwrap();
        eprintln!("chain verification (P-384 leaf + P-384 root, one ECDSA-SHA384 signature): {:.1}ms",
                  ms / 5.0);
        assert!(ms < 15000.0, "a one-link P-384 chain took {}ms", ms / 5.0);
    }
    /// tests/data/chain-google.pem, the capture that named this whole gap, verified the way a
    /// browser would: both links AND the trust store AND a clock. `now` is pinned inside the
    /// captured leaf's window (Sep 4 -- Nov 27 2026) so this keeps meaning the same thing after
    /// the certificate expires; tests/verify.nt checks the two signatures on their own, with no
    /// clock and no store, so a missing system bundle only costs this case and not those.
    /// Before P-384 the second link was refused by algorithm and there was no chain to verify.
    #[test]
    fn the_google_chain_verifies_to_the_system_trust_store() {
        if !std::path::Path::new("/etc/ssl/certs/ca-certificates.crt").exists() {
            eprintln!("no system CA bundle on this machine, skipping");
            return;
        }
        let mut v = verify_vm();
        v.eval("G: x509Parse each pemLoad \"tests/data/chain-google.pem\"").unwrap();
        v.eval("SR: x509SystemRoots[]").unwrap();
        let src = "t0: now`time; r: x509VerifyChain[G; SR; \"www.google.com\"; \
                   (2026.10.01; 12:00:00.000)]; (r; `int$now[`time]-t0)";
        let out = tests::ev(&mut v, src);
        let ms: f64 = out.trim_start_matches("(1b;").trim_end_matches(')').parse()
            .unwrap_or_else(|_| panic!("the Google chain did not verify: {out}"));
        eprintln!("chain verification (www.google.com: P-256/SHA-256 leaf, P-384/SHA-384 \
                   intermediate, system trust store): {ms:.0}ms");
        // and the leaf must not be trusted for a name it does not carry
        let bad = "@[{[x] x509VerifyChain[G; SR; \"www.evil.example\"; (2026.10.01; 12:00:00.000)]}; 0; {x}]";
        assert!(tests::ev(&mut v, bad).starts_with("\"x509: CN=www.google.com is not valid for"),
                "a chain for the wrong host was not refused");
    }
    /// THE ACCEPTANCE TEST, and the only one here that touches the network -- so it is #[ignore]d
    /// and run by hand:  cargo test --release the_public_web -- --ignored --nocapture
    /// Seven hosts, the real system trust store, a real handshake and a real HTTP response. Six of
    /// them were refused before this change, every one at the same place: an ecdsa-with-SHA384
    /// intermediate over a P-384 key. The seventh, www.amazon.com, is RSA and always verified.
    /// A failure here names the host and the sentence it was refused with, which is the whole
    /// point of verify.nt's per-refusal messages.
    #[test]
    #[ignore]
    fn the_public_web_verifies() {
        let mut v = verify_vm();
        v.eval("SR: x509SystemRoots[]").unwrap();
        let mut bad = Vec::new();
        for host in ["www.google.com", "cloudflare.com", "example.com", "github.com",
                     "www.wikipedia.org", "news.ycombinator.com", "www.amazon.com"] {
            let src = format!(
                "@[{{[hh]\n\
                 t0: now`time\n\
                 h: tlsConnectOpts[hh; 443; (enlist `roots)!enlist SR]\n\
                 ms: `int$now[`time]-t0\n\
                 tlsSend[h; \"GET / HTTP/1.0\\r\\nHost: \",hh,\"\\r\\nConnection: close\\r\\n\\r\\n\"]\n\
                 r: `char$tlsRecv h\n\
                 n: count tlsCert h\n\
                 tlsClose h\n\
                 (ms; n; 15#r)}}; \"{host}\"; {{x}}]");
            let out = tests::ev(&mut v, &src).split(" at line ").next().unwrap().to_string();
            eprintln!("{host:>22}  {out}");
            if !out.starts_with('(') { bad.push(format!("{host}: {out}")); }
        }
        assert!(bad.is_empty(), "hosts that did not complete a verified handshake:\n{}", bad.join("\n"));
    }

    /// And how long that costs: two P-256 signature checks (the chain link and the
    /// CertificateVerify) rather than two RSA-2048 exponentiations.
    #[test]
    fn ecdsa_chain_verification_time() {
        let mut v = verify_vm();
        let src = "t0: now`time; do[5; x509VerifyChain[enlist P \"pki-ec-leaf\"; \
                   x509LoadRoots \"tests/data/pki-ec-root.pem\"; \"leaf.neant.test\"; NOW]]; \
                   `int$now[`time]-t0";
        let ms: f64 = tests::ev(&mut v, src).parse().unwrap();
        eprintln!("chain verification (P-256 leaf + P-256 root, one ECDSA signature): {:.1}ms",
                  ms / 5.0);
        assert!(ms < 10000.0, "a one-link ECDSA chain took {}ms", ms / 5.0);
    }

    /// The same server, refused before a byte of application data moves.
    #[test]
    fn a_root_the_store_does_not_hold_is_refused() {
        let srv = s_server("pki-leaf");
        let mut v = verify_vm();
        let store = "x509LoadRoots \"tests/data/selfsigned-rsa.pem\"";
        assert_eq!(talk(&mut v, srv.1, "127.0.0.1", store),
                   format!("'x509: no trusted root named {ROOT}, which issued {INT}"));
    }
    #[test]
    fn a_certificate_that_does_not_cover_the_host_is_refused() {
        // the same good chain to the same trusted root: only the SAN is wrong
        let srv = s_server("pki-wrong-host");
        let mut v = verify_vm();
        assert_eq!(talk(&mut v, srv.1, "127.0.0.1", "RS"),
                   format!("'x509: {LEAF} is not valid for 127.0.0.1 \
                            (subjectAltName: other.neant.test)"));
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

