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

/// Keep large vectors out of mmap.
///
/// glibc's malloc starts sending any block over 128 KB to mmap, and free() gives those straight back
/// to the kernel — so an array language, whose every operation allocates a fresh result, faults in
/// the whole result vector from scratch on every single operation once the vectors get past 16k
/// int64s. Measured on `abs` over 100k ints: 1,256,235 minor page faults and 3.79 ns per element,
/// against 2,506 and 1.10 with the threshold raised. That is 3.4x, for arithmetic that never
/// touched the disk or the network.
///
/// The threshold is normally raised by glibc itself, but only once it has seen an mmap'd block
/// freed, so whether a program pays this depends on whether something earlier happened to allocate
/// and release a big enough temporary. That is not a thing to leave to chance.
///
/// Declared here rather than pulled in with the libc crate because this tree has no dependencies and
/// that is worth more than the five lines. glibc only: mallopt is not in POSIX, and musl does not
/// have it, so the symbol has to be absent from the link on anything else.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn tune_allocator() {
    unsafe extern "C" { fn mallopt(param: i32, value: i32) -> i32; }
    const M_TRIM_THRESHOLD: i32 = -1;
    const M_MMAP_THRESHOLD: i32 = -3;
    unsafe {
        mallopt(M_MMAP_THRESHOLD, 512 * 1024 * 1024);
        mallopt(M_TRIM_THRESHOLD, 512 * 1024 * 1024);
    }
}
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn tune_allocator() {}

fn main() {
    tune_allocator();
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
    /// The check tests/tlsserver.nt cannot make: that a real TLS implementation accepts what this
    /// server sends. `openssl s_client` handshakes against it, verifies the fixture chain to the
    /// fixture root, and echoes a line back — so a ServerHello, a Certificate, a CertificateVerify
    /// signature or a Finished this build got subtly wrong fails here rather than only when talking
    /// to itself. Needs a subprocess, which is why it is Rust.
    #[test]
    fn openssl_client_completes_our_handshake() {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let mut v = boot_vm();
        for f in ["src/neant/crypto/tlsclient.nt", "src/neant/crypto/sign.nt", "src/neant/crypto/tlsserver.nt"] {
            v.eval(&std::fs::read_to_string(f).unwrap()).unwrap_or_else(|e| panic!("{f}: '{}", e.0));
        }
        v.eval("certs: pemLoad \"tests/data/pki-sha512-leaf.pem\"; pk: rsaKeyLoad \"tests/data/pki-sha512-leaf.key\"").unwrap();
        v.eval(&format!("l: hlisten \"127.0.0.1:{port}\"")).unwrap();
        let t = std::thread::spawn(move || {
            v.eval("c: accept l; h: tlsAccept[c; certs; pk]; m: tlsRecv h; tlsSend[h; (`byte$\"echo:\"),m]; tlsClose h; `char$m")
                .map(|r| r.fmt()).unwrap_or_else(|e| format!("'{}", e.0))
        });
        std::thread::sleep(std::time::Duration::from_millis(200));

        let out = std::process::Command::new("openssl")
            .args(["s_client", "-connect", &format!("127.0.0.1:{port}"), "-tls1_3",
                   "-CAfile", "tests/data/pki-sha512-root.pem", "-servername", "leaf.neant.test",
                   "-verify_return_error", "-quiet"])
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn().expect("openssl s_client");
        use std::io::Write as _;
        out.stdin.as_ref().unwrap().write_all(b"hello from openssl\n").unwrap();
        let done = out.wait_with_output().unwrap();
        let said = String::from_utf8_lossy(&done.stdout).to_string();
        let err = String::from_utf8_lossy(&done.stderr).to_string();
        assert!(said.contains("echo:hello from openssl"), "s_client stdout {said:?} stderr {err:?}");
        let saw = t.join().unwrap();
        assert!(saw.contains("hello from openssl"), "the server read {saw:?}");
    }

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
        // `Connection: close`, because this reads to EOF: httpServe keeps a 1.1 connection open
        // otherwise, which is the point of it and would hang this read forever.
        c.write_all(b"GET /hello HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n").unwrap();
        let mut resp = String::new();
        c.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 200 OK\r\n"), "{resp:?}");
        assert!(resp.contains("content-type: text/plain\r\n"), "{resp:?}");
        assert!(resp.ends_with("method=GET path=/hello body="), "{resp:?}");

        let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let body = "hi there";
        c.write_all(format!("POST /echo HTTP/1.1\r\nHost: h\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).unwrap();
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
/// ASN.1 DER and X.509 certificate parsing (src/neant/crypto/{der,x509}.nt), for the one case that
/// needs Rust: pemDecode is fed a wrapper with embedded newlines, which is a `format!` here and an
/// escaping problem in a .nt file. Everything else -- every universal type a certificate uses,
/// every encoding DER forbids, the three openssl fixtures field by field, the captured chain and
/// the malformed blocks -- is pure neant and lives in tests/x509.nt, which tests::nt_tests runs.
/// Neither module is in the boot image, so both are loaded into a plain boot VM.
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

/// RSA signature verification (src/neant/crypto/rsa.nt), for the one thing that needs the host: how
/// long one verification takes. Everything else -- the limb layer (tests/bignum.nt), the openssl
/// signatures at three salt lengths, every tampered and malformed signature that must come back 0b,
/// Bleichenbacher '06 and the PSS encoding rules -- is pure neant and lives in tests/bignum.nt and
/// tests/rsa.nt, which tests::nt_tests runs.
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

    /// n, e, the digest and one signature, bound in the VM so a test reads as one line.
    fn key(sig: &str) -> String {
        format!("n: unhex \"{N}\"; e: 0x010001; dg: unhex \"{DIGEST}\"; s: unhex \"{sig}\"; ")
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

