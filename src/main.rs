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

const BOOT_FILES: [&str; 11] = [
    "src/neant/stdlib/prelude.nt", "src/neant/core/lex.nt", "src/neant/core/parse.nt", "src/neant/core/compile.nt",
    "src/neant/stdlib/table.nt", "src/neant/stdlib/json.nt", "src/neant/stdlib/encode.nt", "src/neant/stdlib/regex.nt",
    "src/neant/stdlib/test.nt", "src/neant/crypto/crypto.nt", "src/neant/jit/arm64.nt",
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

    /// Calls the same closure enough times to cross the tier-up threshold partway through, and
    /// checks every call — interpreted and compiled alike — agrees on the result.
    #[test]
    fn compiled_and_interpreted_agree() {
        let mut v = boot_vm();
        let got = v.eval(
            "f: {[k] n:0; i:0; while[i<k; n: n+i*i; i: i+1]; n}; \
             r: (); i: 0; while[i<100; r,: f 1000; i+:1]; \
             distinct r",
        ).unwrap();
        assert_eq!(got.fmt(), ",332833500"); // one distinct value across 100 calls
    }

    /// `f`'s own cold run stopped being an interpreted baseline once the tracing JIT landed — a
    /// single call is enough for it to take the loop over partway through. `fSlow` is the same
    /// loop with two monadic negations (`- -`, an identity) spliced in: `Op::Monad` is rejected by
    /// both tiers, so it stays interpreted however many iterations it runs. (A `do` loop used to
    /// serve as this baseline, being the one loop form no trace was started on; since `do`
    /// headers trace too, the `- -` twin every differential test already uses is the only shape
    /// left that is guaranteed interpreted.) The twin does marginally *more* work per iteration
    /// than `f` — two extra dispatches — so this overstates the ratio by a few percent, which is
    /// the same direction every other manual_* number here is biased in.
    #[test]
    #[ignore]
    fn manual_perf_measurement() {
        let mut v = boot_vm();
        v.eval("f: {[k] n:0; i:0; while[i<k; n: n+i*i; i: i+1]; n}").unwrap();
        v.eval("fSlow: {[k] n:0; i:0; while[i<k; n: n+(- - i*i); i: i+1]; n}").unwrap();
        let t0 = std::time::Instant::now();
        v.eval("fSlow 1000000").unwrap();
        let cold = t0.elapsed();
        for _ in 0..70 { v.eval("f 10").unwrap(); } // cross the tier-up threshold
        let t1 = std::time::Instant::now();
        v.eval("f 1000000").unwrap();
        let hot = t1.elapsed();
        println!("interpreted {cold:?}  compiled {hot:?}  ratio {:.1}x", cold.as_secs_f64() / hot.as_secs_f64());
    }

    /// `do[n;..]` (`Op::Loop`) is the one op with an asymmetric stack-depth delta between its two
    /// edges (fall through unchanged, exit pops one) — nested loops are the case that would catch
    /// a depth-bookkeeping mistake in the compiler itself, not just a codegen mistake.
    #[test]
    fn do_loop_compiled_and_interpreted_agree() {
        let mut v = boot_vm();
        let got = v.eval(
            "f: {[k] n:0; do[k; do[3; n: n+1]]; n}; \
             r: (); i: 0; while[i<100; r,: f 10; i+:1]; \
             distinct r",
        ).unwrap();
        assert_eq!(got.fmt(), ",30");
    }

    /// A single top-level call to a recursive function is itself hundreds of thousands of calls
    /// through `call_code` for `fib(27)`-sized input, so it crosses the tier-up threshold within
    /// its own first invocation — there's no way to time a "cold" `fib` against itself. Instead,
    /// compare against `fibSlow`, the identical algorithm with two monadic negations (`- -`, an
    /// identity) spliced in purely to make it permanently uncompilable (`Op::Monad` is never
    /// accepted) — same result, same recursive shape, guaranteed interpreted throughout, so the
    /// ratio isolates what compiling the calls themselves is worth.
    #[test]
    #[ignore]
    fn manual_recursive_perf_measurement() {
        let mut v = boot_vm();
        v.eval("fib: {$[x<2;x;fib[x-1]+fib[x-2]]}").unwrap();
        v.eval("fibSlow: {$[x<2;x;(- - fibSlow[x-1])+(- - fibSlow[x-2])]}").unwrap();
        for _ in 0..70 { v.eval("fib 5").unwrap(); } // cross the tier-up threshold
        let t0 = std::time::Instant::now();
        let hot = v.eval("fib 27").unwrap();
        let hot_time = t0.elapsed();
        let t1 = std::time::Instant::now();
        let slow = v.eval("fibSlow 27").unwrap();
        let slow_time = t1.elapsed();
        assert_eq!(hot.fmt(), slow.fmt());
        println!("interpreted {slow_time:?}  compiled {hot_time:?}  ratio {:.1}x", slow_time.as_secs_f64() / hot_time.as_secs_f64());
    }

    /// `x*fact(x-1)`: `x` is live across the recursive call, so a compiler that spills/reloads the
    /// wrong registers around a call would silently corrupt this even while getting simpler,
    /// non-live-across-call recursion right.
    #[test]
    fn recursive_call_compiled_and_interpreted_agree() {
        let mut v = boot_vm();
        let got = v.eval(
            "fact: {$[x<2;1;x*fact x-1]}; \
             r: (); i: 0; while[i<100; r,: fact 15; i+:1]; \
             distinct r",
        ).unwrap();
        assert_eq!(got.fmt(), ",1307674368000"); // 15!
    }

    /// Two recursive calls in one function — the trampoline used twice per invocation, and twice
    /// as much live-register pressure across each call as the `fact` case.
    #[test]
    fn double_recursive_call_compiled_and_interpreted_agree() {
        let mut v = boot_vm();
        let got = v.eval(
            "fib: {$[x<2;x;fib[x-1]+fib[x-2]]}; \
             r: (); i: 0; while[i<100; r,: fib 20; i+:1]; \
             distinct r",
        ).unwrap();
        assert_eq!(got.fmt(), ",6765");
    }

    /// Runaway recursion through compiled calls goes through `blr`, not `Vm::call_code`'s own
    /// `self.depth` guard — without its own limit (`MAX_CALL_DEPTH`, src/jit.rs) this would blow
    /// the real machine stack instead of failing like every other kind of infinite recursion here.
    #[test]
    fn deep_recursion_fails_cleanly_instead_of_overflowing_the_stack() {
        let mut v = boot_vm();
        v.eval("h: {[x] $[x<1; 0; 1+h[x-1]]}; i:0; while[i<80; r: h 500; i+:1]").unwrap();
        assert_eq!(tests::ev(&mut v, "h 1000000"), "'stack: recursion too deep");
    }

    /// Calling an impure function (here, one that mutates a `shared` cell) from a hot caller must
    /// never fire that side effect twice. The trampoline (src/jit.rs `jit_call`) proves a callee is
    /// just as pure as the caller *before* calling it — `supd` resolves to a `Prim`, not a
    /// `Lambda`/`Closure`, so it's never actually reached through the compiled path at all, and
    /// every one of these calls runs on the interpreter instead, exactly once each.
    #[test]
    fn impure_callee_never_fires_its_side_effect_twice() {
        let mut v = boot_vm();
        v.eval("s: shared 0; bump: {[x] supd[s;{x+1}]; x+1}; f: {[x] bump x}").unwrap();
        for i in 0..80 { v.eval(&format!("f {i}")).unwrap(); }
        assert_eq!(tests::ev(&mut v, "sget s"), "80");
    }

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

    /// `x[i]` on a vector *parameter* is the one pattern `src/neant/jit/arm64.nt`'s
    /// `jitClassifySlots` recognizes as compilable (see README "Stage 2") — a `LoadL` immediately
    /// consumed by `Call(1)`, the same shape `x[i]` and plain application (`f x`) both compile to.
    #[test]
    fn vector_index_read_compiled_and_interpreted_agree() {
        let mut v = boot_vm();
        let got = v.eval(
            "f: {[x;n] s:0; i:0; while[i<n; s: s+x[i]; i: i+1]; s}; \
             r: (); i: 0; while[i<100; r,: f[1 2 3 4 5 6 7 8 9 10; 10]; i+:1]; \
             distinct r",
        ).unwrap();
        assert_eq!(got.fmt(), ",55");
    }

    /// `x[i]:v` on a vector parameter — `TakeL;Amend;StoreL`, always emitted back to back for a
    /// single-index amend. Returns `x[n-1]` (a scalar), not `x` itself: nothing in this scheme can
    /// return a vector — see `jitOpVecSet`'s doc comment on why that's fine, the write only needs
    /// to be visible to *later reads within the same compiled call*, exactly like this one.
    #[test]
    fn vector_index_write_compiled_and_interpreted_agree() {
        let mut v = boot_vm();
        let got = v.eval(
            "f: {[x;n] i:0; while[i<n; x[i]: x[i]*2; i: i+1]; x[n-1]}; \
             r: (); i: 0; while[i<100; r,: f[1 2 3 4 5; 5]; i+:1]; \
             distinct r",
        ).unwrap();
        assert_eq!(got.fmt(), ",10");
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

    /// The one thing that would be silently *wrong*, not loud, if `jit_vec_set` (src/jit.rs) ever
    /// skipped its `Arc::make_mut` check: a second live binding to the same vector observing a
    /// write it never asked for. `try_run`'s own clone into `vecbuf` guarantees the refcount is
    /// already >1 by the time any write happens, so this is exercised on every single call, not
    /// just this test — but this is the one that would actually catch a broken check, the same
    /// role `impure_callee_never_fires_its_side_effect_twice` plays for the call path.
    #[test]
    fn vector_write_never_corrupts_a_live_second_reference() {
        let mut v = boot_vm();
        v.eval("f: {[x;n] i:0; while[i<n; x[i]: x[i]*2; i: i+1]; x[0]}").unwrap();
        for _ in 0..70 { v.eval("f[1 2 3; 3]").unwrap(); } // cross the tier-up threshold
        assert_eq!(
            tests::ev(&mut v, "orig: 1 2 3; also: orig; r: f[orig;3]; (r; orig; also)"),
            "(2;1 2 3;1 2 3)",
        );
    }

    /// Every local of a compiled function lives in a register for the whole call (`jitLREGS`,
    /// src/neant/jit/arm64.nt): the first six in callee-saved ones, the next five in caller-saved
    /// ones spilled around every trampoline call, and any beyond that back in the interpreter's
    /// buffer. `g` has fourteen locals, all live across its recursive call, so one of each class is
    /// held across a `blr` here — a wrong pair in the prologue/epilogue saves, a caller-saved local
    /// not spilled around the call, or one register handed to two slots would each change this
    /// sum, while `fact`/`fib` above (one local) would still come out right. `gSlow` is the same
    /// function kept interpreted by `- -` (see `manual_recursive_perf_measurement`) and is the
    /// oracle; `distinct` over both proves all 100 compiled-or-not calls agree with it.
    #[test]
    fn many_locals_survive_a_recursive_call_in_every_register_class() {
        let mut v = boot_vm();
        let got = v.eval(
            "g: {[x] if[x<1; :0]; a:x+1; b:x+2; c:x+3; d:x+4; e:x+5; f:x+6; h:x+7; k:x+8; m:x+9; p:x+10; q:x+11; r:x+12; s: g[x-1]; a+b+c+d+e+f+h+k+m+p+q+r+s}; \
             gSlow: {[x] if[x<1; :0]; a:x+(- - 1); b:x+2; c:x+3; d:x+4; e:x+5; f:x+6; h:x+7; k:x+8; m:x+9; p:x+10; q:x+11; r:x+12; s: gSlow[x-1]; a+b+c+d+e+f+h+k+m+p+q+r+s}; \
             r: (); i: 0; while[i<100; r,: g 10; i+:1]; r,: gSlow 10; \
             distinct r",
        ).unwrap();
        assert_eq!(got.fmt(), ",1440"); // sum over x=1..10 of 12x+78
    }

    /// A vector param's slot holds a pointer into `vecbuf` (src/jit.rs) rather than an int, and it
    /// gets a register like any other local — which one depends on first use in the bytecode. In
    /// `f` (nine locals) the vector's first use comes after seven int locals, so its pointer sits in
    /// a caller-saved register that has to be spilled around the very `jit_vec_get` call it is the
    /// argument to; in `w` (fourteen locals, with a write as well as a read) it's past the last
    /// register and stays in the buffer, reached through x19 as before. A pointer clobbered by the
    /// trampoline, or an int local sharing its register, would fault or change the sum on the next
    /// iteration. `n` (10) stays under the trace threshold so the whole-function JIT is what runs
    /// these loops; the `- -` twins are the interpreted oracles.
    #[test]
    fn vector_param_and_many_int_locals_share_the_register_file_correctly() {
        let mut v = boot_vm();
        let got = v.eval(
            "f: {[x;n] s:0; a:0; b:0; c:0; d:0; e:0; i:0; while[i<n; s: s+x[i]; a: a+1; b: b+i; c: c+2; d: d+3; e: e+s; i: i+1]; s+a+b+c+d+e}; \
             fSlow: {[x;n] s:0; a:0; b:0; c:0; d:0; e:0; i:0; while[i<n; s: s+x[i]; a: a+(- - 1); b: b+i; c: c+2; d: d+3; e: e+s; i: i+1]; s+a+b+c+d+e}; \
             w: {[x;n] s:0; a:0; b:0; c:0; d:0; e:0; g:0; h:0; k:0; m:0; p:0; i:0; while[i<n; x[i]: x[i]*2; s: s+x[i]; a: a+1; b: b+i; c: c+2; d: d+3; e: e+s; g: g+4; h: h+5; k: k+6; m: m+7; p: p+8; i: i+1]; s+a+b+c+d+e+g+h+k+m+p}; \
             wSlow: {[x;n] s:0; a:0; b:0; c:0; d:0; e:0; g:0; h:0; k:0; m:0; p:0; i:0; while[i<n; x[i]: x[i]*2; s: s+x[i]; a: a+(- - 1); b: b+i; c: c+2; d: d+3; e: e+s; g: g+4; h: h+5; k: k+6; m: m+7; p: p+8; i: i+1]; s+a+b+c+d+e+g+h+k+m+p}; \
             xs: 1 2 3 4 5 6 7 8 9 10; \
             rf: (); rw: (); i: 0; while[i<100; rf,: f[xs;10]; rw,: w[xs;10]; i+:1]; rf,: fSlow[xs;10]; rw,: wSlow[xs;10]; \
             (distinct rf; distinct rw; xs)",
        ).unwrap();
        // `xs` unchanged too: the in-loop write went to `w`'s own copy (see jit_vec_set).
        assert_eq!(got.fmt(), "(,380;,955;1 2 3 4 5 6 7 8 9 10)");
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

    /// A `while` loop goes hot *inside a single call* — the tracing JIT counts backward jumps per
    /// loop header, not calls per function (`FnCode::loop_action`, src/value.rs) — so one `f 1000`
    /// crosses the threshold partway through and runs the rest of its iterations natively.
    /// `fSlow` is the same loop with two monadic negations (`- -`, an identity) spliced in purely
    /// to keep it permanently untraceable (`Op::Monad` is outside Milestone 1's scope,
    /// src/trace.rs), so it stays interpreted and is the oracle. The sizes straddle the threshold
    /// in both directions: below it nothing is ever recorded, just above it the trace is compiled
    /// and immediately has only a few iterations left to run, well above it almost the whole loop
    /// is native.
    #[test]
    fn traced_loop_and_interpreted_agree() {
        let mut v = boot_vm();
        let got = v.eval(
            "f: {[k] n:0; i:0; while[i<k; n: n+i*i; i: i+1]; n}; \
             fSlow: {[k] n:0; i:0; while[i<k; n: n+(- - i*i); i: i+1]; n}; \
             {[k] (f k)=fSlow k} each 0 1 63 64 65 66 200 1000",
        ).unwrap();
        assert_eq!(got.fmt(), "11111111b");
    }

    /// Every branch the recorded iteration took is a guard fixed to *that* direction, so a
    /// condition that flips later is the case where compiled code has to hand control back
    /// mid-loop and let the interpreter take the other edge — here on the iteration after the
    /// 100th, long after the trace was recorded on the 64th. `g` also stores to `n` *before* its
    /// guard, which is what makes it a test of the bail handing back current locals rather than
    /// the ones the iteration started with.
    #[test]
    fn traced_loop_guards_bail_when_a_branch_flips() {
        let mut v = boot_vm();
        let got = v.eval(
            "f: {[k] n:0; i:0; while[i<k; n: $[i<100; n+2; n+1]; i: i+1]; n}; \
             fSlow: {[k] n:0; i:0; while[i<k; n: $[i<100; n+(- - 2); n+(- - 1)]; i: i+1]; n}; \
             g: {[k] n:0; i:0; while[i<k; n: n+1; if[n>100; n: n+10]; i: i+1]; n}; \
             gSlow: {[k] n:0; i:0; while[i<k; n: n+(- - 1); if[n>100; n: n+10]; i: i+1]; n}; \
             ({[k] (f k)=fSlow k} each 99 100 101 300), {[k] (g k)=gSlow k} each 99 100 101 300",
        ).unwrap();
        assert_eq!(got.fmt(), "11111111b");
    }

    /// Floats run on a second operand stack of their own (`jitFSTACK`, d16..d21) with its own
    /// encoders, and a trace is the only part of this JIT that sees them at all — the whole-
    /// function JIT rejects anything non-int outright. `&` is in here specifically because it
    /// compiles to FMINNM, not FMIN (see `jitFDyad`'s comment for why that distinction is real).
    #[test]
    fn traced_float_loop_and_interpreted_agree() {
        let mut v = boot_vm();
        let got = v.eval(
            "f: {[k] s:0.0; i:0; while[i<k; s: (s+0.5)&1000.0; i: i+1]; s}; \
             fSlow: {[k] s:0.0; i:0; while[i<k; s: (s+(- - 0.5))&1000.0; i: i+1]; s}; \
             {[k] (f k)=fSlow k} each 63 64 65 200 5000",
        ).unwrap();
        assert_eq!(got.fmt(), "11111b");
    }

    /// `0W+1` wraps to exactly the int-null sentinel — the same case
    /// `deopts_on_null_collision_instead_of_diverging` covers for the whole-function JIT, but a
    /// trace can only hit it *mid-loop*, with earlier iterations' writes already committed. The
    /// exit hands the interpreter the locals as they stand and the operand stack at the failing
    /// op — here just the null result — and resumes at the op after the `Dyad`
    /// (`TraceOp::Dyad`'s `resume_ip`, src/trace.rs; `jitTrDyad`, src/neant/jit/arm64.nt), so the
    /// interpreter's own `StoreL` puts the null in `n` and propagates it from there. Starting at
    /// `0W-200` is what puts the collision ~200 iterations in — well after the trace was recorded
    /// and compiled, which starting at `0W` would not (`n` would already be null by then, and a
    /// null local is refused at the recorder).
    #[test]
    fn traced_loop_deopts_on_null_collision_instead_of_diverging() {
        let mut v = boot_vm();
        let got = v.eval(
            "f: {[k] n: 0W-200; i:0; while[i<k; n: n+1; i+:1]; n}; \
             fSlow: {[k] n: 0W-200; i:0; while[i<k; n: n+(- - 1); i+:1]; n}; \
             ({[k] (f k)~fSlow k} each 100 199 200 201 1000), (,(f 1000)~0N)",
        ).unwrap();
        assert_eq!(got.fmt(), "111111b");
    }

    /// A trace's locals round-trip through a buffer of raw 64-bit words, so the type they are
    /// rebuilt as on the way out is the type they have afterwards. A `Bool` local rebuilt as an
    /// `Int` would be a silent, observable change (`type`, `show`, `string` all see it), so the
    /// recorder refuses to trace a loop that stores one at all (`TraceTy::of_local`,
    /// src/trace.rs) — this is what would catch that check going missing.
    #[test]
    fn a_bool_local_is_still_a_bool_after_a_traced_loop() {
        let mut v = boot_vm();
        assert_eq!(tests::ev(&mut v, "{[k] i:0; b: 0b; while[i<k; b: i<100; i: i+1]; (type b; b)} 200"), "(`bool;0b)");
    }

    /// The inner loop is the one that goes hot (its header is reached `k*3` times), and recording
    /// the *outer* one fails on the inner loop's own backward jump — a `Jmp` that isn't this
    /// trace's header ends the recording without closing it (src/trace.rs). Both loops writing
    /// the same local is what would catch a trace that wrote its locals back at the wrong moment.
    #[test]
    fn traced_nested_loop_and_interpreted_agree() {
        let mut v = boot_vm();
        let got = v.eval(
            "f: {[k] n:0; i:0; while[i<k; j:0; while[j<3; n: n+i; j: j+1]; i: i+1]; n}; \
             fSlow: {[k] n:0; i:0; while[i<k; j:0; while[j<3; n: n+(- - i); j: j+1]; i: i+1]; n}; \
             {[k] (f k)=fSlow k} each 10 64 100 500",
        ).unwrap();
        assert_eq!(got.fmt(), "1111b");
    }

    /// Each local a trace touches gets a register of its own for the whole loop, handed out from
    /// two independent files (`jitTrLOCALS`/`jitTrFLOCALS`, src/neant/jit/arm64.nt) — so a loop
    /// mixing both kinds is the case that would catch the two allocations colliding, and one with
    /// more locals than either file holds is the case that has to give up cleanly rather than
    /// index past the end of it. `f` has 12 int locals against a file of 8; `g` fills the float
    /// file exactly.
    #[test]
    fn traced_loop_allocates_a_register_per_local() {
        let mut v = boot_vm();
        let got = v.eval(
            "h: {[k] x:0.0; y:1.0; n:0; i:0; while[i<k; x: x+0.25; y: y*1.5; n: n+i; i: i+1]; (x;y;n)}; \
             hSlow: {[k] x:0.0; y:1.0; n:0; i:0; while[i<k; x: x+(- - 0.25); y: y*1.5; n: n+i; i: i+1]; (x;y;n)}; \
             f: {[k] a:0;b:0;c:0;d:0;e:0;g:0;m:0;p:0;q:0;r:0; i:0; while[i<k; a: a+1; b: b+2; c: c+3; d: d+4; e: e+5; g: g+6; m: m+7; p: p+8; q: q+9; r: r+10; i: i+1]; a+b+c+d+e+g+m+p+q+r}; \
             fSlow: {[k] a:0;b:0;c:0;d:0;e:0;g:0;m:0;p:0;q:0;r:0; i:0; while[i<k; a: a+(- - 1); b: b+2; c: c+3; d: d+4; e: e+5; g: g+6; m: m+7; p: p+8; q: q+9; r: r+10; i: i+1]; a+b+c+d+e+g+m+p+q+r}; \
             ({[k] (h k)~hSlow k} each 63 64 65 200), {[k] (f k)~fSlow k} each 63 64 65 200",
        ).unwrap();
        assert_eq!(got.fmt(), "11111111b");
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

    /// One call each — the whole-function JIT never enters into it (that needs 64) — against the
    /// same `- -` twin `manual_perf_measurement` above uses, and for the same reason: a `do` loop
    /// is no longer an interpreted baseline now that `do` headers trace. The body of the loop
    /// this measures has no branch in it and no call, so its trace has no rewind exit and
    /// therefore no store in it at all (`jitCompileTrace`, src/neant/jit/arm64.nt) — this is the
    /// number to compare against the one before the operand-stack handoff landed, when the loop
    /// still stored its written locals once per iteration (5.6ms then; see README "Stage 2b").
    #[test]
    #[ignore]
    fn manual_trace_perf_measurement() {
        let mut v = boot_vm();
        v.eval("f: {[k] n:0; i:0; while[i<k; n: n+i*i; i: i+1]; n}").unwrap();
        v.eval("fSlow: {[k] n:0; i:0; while[i<k; n: n+(- - i*i); i: i+1]; n}").unwrap();
        let t0 = std::time::Instant::now();
        let slow = v.eval("fSlow 5000000").unwrap();
        let interpreted = t0.elapsed();
        let t1 = std::time::Instant::now();
        let fast = v.eval("f 5000000").unwrap();
        let traced = t1.elapsed();
        assert_eq!(slow.fmt(), fast.fmt());
        println!("interpreted {interpreted:?}  traced {traced:?}  ratio {:.1}x", interpreted.as_secs_f64() / traced.as_secs_f64());
    }

    /// The simplest call a loop can make — one plain lambda, one int argument, straight-line body
    /// — inlined into the trace (`Recorder::enter_frame`, src/trace.rs): the callee's frame is
    /// recorded as `FramePush`, a `StoreL`+`Pop` binding its argument into a slot past the loop's
    /// own frame, its body, and `FrameEnd`. The argument slot is *virtual* (at or past
    /// `real_upto`) — never loaded from or written back to the interpreter's frame — so this is
    /// also what would catch a trace writing a callee's local over one of the loop's own. Sizes
    /// straddle the 64-iteration threshold at which the trace is recorded.
    #[test]
    fn traced_call_and_interpreted_agree() {
        let mut v = boot_vm();
        let got = v.eval(
            "f: {x*2}; \
             g: {[k] s:0; i:0; while[i<k; s: s+f i; i: i+1]; s}; \
             gSlow: {[k] s:0; i:0; while[i<k; s: s+(- - f i); i: i+1]; s}; \
             {[k] (g k)=gSlow k} each 0 1 63 64 65 66 200 1000",
        ).unwrap();
        assert_eq!(got.fmt(), "11111111b");
    }

    /// `Op::Call` pops the callee, then parameter 0, then parameter 1 — so the first argument is
    /// the *top* of the stack when the frame opens, and `enter_frame` binds top-down in that order.
    /// `a*b+1` — `a*(b+1)`, right to left — is deliberately asymmetric in its two arguments:
    /// `m[i;3]` summed over `i<10` is 180, and with the arguments swapped it would be 165, which
    /// is exactly what a binding in the wrong order would produce.
    #[test]
    fn traced_two_arg_call_binds_arguments_in_order() {
        let mut v = boot_vm();
        let got = v.eval(
            "m: {[a;b] a*b+1}; \
             g: {[k] s:0; i:0; while[i<k; s: s+m[i;3]; i: i+1]; s}; \
             gSlow: {[k] s:0; i:0; while[i<k; s: s+(- - m[i;3]); i: i+1]; s}; \
             ({[k] (g k)=gSlow k} each 63 64 65 200), (,180=g 10)",
        ).unwrap();
        assert_eq!(got.fmt(), "11111b");
    }

    /// A branch *inside* a callee can't bail to the branch's other edge the way one in the loop's
    /// own frame does — that `ip` is in the callee's bytecode, and the interpreter is not in that
    /// frame — so it is a `GuardRewind` (src/trace.rs): abandon the half-finished iteration and
    /// resume interpreting at the loop header from the values it started with. Here the callee's
    /// condition holds for the first 100 iterations (which is when the trace is recorded) and
    /// flips after, so from `i=101` on every entry rewinds, the iteration runs interpreted, and
    /// the next backward jump re-enters the trace — which then rewinds again. `k=99` never flips.
    #[test]
    fn traced_call_rewinds_when_a_callee_branch_flips() {
        let mut v = boot_vm();
        let got = v.eval(
            "ab: {$[x<0;0-x;x]}; \
             g: {[k] s:0; i:0; while[i<k; s: s+ab[100-i]; i: i+1]; s}; \
             gSlow: {[k] s:0; i:0; while[i<k; s: s+(- - ab[100-i]); i: i+1]; s}; \
             {[k] (g k)=gSlow k} each 63 64 65 99 100 101 102 300",
        ).unwrap();
        assert_eq!(got.fmt(), "11111111b");
    }

    /// An early `:x` in a callee is an `Op::Ret` mid-frame: the interpreter leaves `run_ops`
    /// there, so the recorder never sees the op at all — `Vm::call_code`'s `exit_frame` is what
    /// closes the frame, and `FrameEnd` takes whatever is on top of the trace's stack as the
    /// result. The `if` in front of it is a `GuardRewind`, so this also covers the return being
    /// *skipped* for the first 100 iterations and *taken* after.
    #[test]
    fn traced_call_with_early_return_and_interpreted_agree() {
        let mut v = boot_vm();
        let got = v.eval(
            "er: {if[x<0; :0]; x*3}; \
             g: {[k] s:0; i:0; while[i<k; s: s+er[100-i]; i: i+1]; s}; \
             gSlow: {[k] s:0; i:0; while[i<k; s: s+(- - er[100-i]); i: i+1]; s}; \
             {[k] (g k)=gSlow k} each 63 64 65 100 101 300",
        ).unwrap();
        assert_eq!(got.fmt(), "111111b");
    }

    /// Frames nest: `f[h[i]]` has `h`'s frame open and closed *while `f`'s argument is being
    /// built* (the compiler emits arguments before the callee, `gen`, src/neant/core/compile.nt),
    /// and `c` calls `h` from inside its own body, so its frame is open when `h`'s opens. Each
    /// frame gets its own trace-local slot range (`next_base`) and its own entry in `frames`; a
    /// result left in the wrong register by `FrameEnd` — or the two frames' slots colliding —
    /// gives a wrong sum here. Two distinct callees also means two entries in the entry guard.
    #[test]
    fn traced_nested_calls_and_interpreted_agree() {
        let mut v = boot_vm();
        let got = v.eval(
            "f: {x*2}; h: {x+1}; c: {h[x]*3}; \
             g: {[k] s:0; i:0; while[i<k; s: s+f[h[i]]; i: i+1]; s}; \
             gSlow: {[k] s:0; i:0; while[i<k; s: s+(- - f[h[i]]); i: i+1]; s}; \
             g2: {[k] s:0; i:0; while[i<k; s: s+c i; i: i+1]; s}; \
             g2Slow: {[k] s:0; i:0; while[i<k; s: s+(- - c i); i: i+1]; s}; \
             ({[k] (g k)=gSlow k} each 63 64 65 200), {[k] (g2 k)=g2Slow k} each 63 64 65 200",
        ).unwrap();
        assert_eq!(got.fmt(), "11111111b");
    }

    /// A float argument is bound the same way an int one is, into the float register file, and
    /// the callee's result comes back on the float operand stack — `FrameEnd` has to move it by
    /// the float file's numbering, not the int one's. `x` counts up in floats so nothing here
    /// mixes types (an int/float `Dyad` is out of scope for a trace, calls or not).
    #[test]
    fn traced_float_call_and_interpreted_agree() {
        let mut v = boot_vm();
        let got = v.eval(
            "ff: {x*1.5}; \
             g: {[k] s:0.0; x:0.0; i:0; while[i<k; s: s+ff x; x: x+1.0; i: i+1]; s}; \
             gSlow: {[k] s:0.0; x:0.0; i:0; while[i<k; s: s+(- - ff x); x: x+1.0; i: i+1]; s}; \
             {[k] (g k)=gSlow k} each 63 64 65 200 5000",
        ).unwrap();
        assert_eq!(got.fmt(), "11111b");
    }

    /// A callee's own scratch local is another virtual slot — written every iteration, never
    /// loaded on entry (it has no value before the loop) and never written back (nobody wants it
    /// after). `jitTraceTouched` (src/neant/jit/arm64.nt) keeps it out of the `stored` list, so
    /// this is the case that would catch a virtual slot being stored to the buffer, or worse,
    /// written back into the interpreter's frame at a slot index the loop's own frame doesn't have.
    #[test]
    fn traced_call_with_a_scratch_local_and_interpreted_agree() {
        let mut v = boot_vm();
        let got = v.eval(
            "sq: {[x] t: x+1; t*t}; \
             g: {[k] s:0; i:0; while[i<k; s: s+sq i; i: i+1]; s}; \
             gSlow: {[k] s:0; i:0; while[i<k; s: s+(- - sq i); i: i+1]; s}; \
             {[k] (g k)=gSlow k} each 63 64 65 200",
        ).unwrap();
        assert_eq!(got.fmt(), "1111b");
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

    /// Which lambda a global holds is checked once per entry (`CompiledTrace::run`'s `callees`,
    /// src/jit.rs), and a trace whose callee has been reassigned is retired rather than refused
    /// forever: the header counts afresh and is recorded again against the new definition
    /// (`FnCode::retrace`, src/value.rs), up to `MAX_RETRACE` times, after which the loop simply
    /// stays interpreted. Every run here has to follow the *current* `f` — the first two are the
    /// original trace and its replacement, the last two are past the budget — and a trace that
    /// kept running the old body would give the old answer.
    #[test]
    fn traced_call_follows_a_reassigned_global() {
        let mut v = boot_vm();
        let got = v.eval(
            "g: {[k] s:0; i:0; while[i<k; s: s+f i; i: i+1]; s}; \
             f: {x*2}; r: ,g 200; f: {x*3}; r,: g 200; f: {x*4}; r,: g 200; f: {x*5}; r,: g 200; \
             f: {x*6}; r,: g 200; f: {x*7}; r,: g 200; f: {x*8}; r,: g 200; \
             r=19900*2 3 4 5 6 7 8",
        ).unwrap();
        assert_eq!(got.fmt(), "1111111b");
    }

    /// Everything a call site can name that is *not* a plain lambda held by a global: a closure
    /// (`Value::Closure`, not `Lambda`), a primitive (`Op::Call`'s own fast path — no frame is
    /// ever entered, so the `Call` arrives at the recorder with nothing `returned`), a projection
    /// (`call_code` returns before `enter_frame`), an arity mismatch (which *is* a projection),
    /// and a callee that reads a global (`LoadG` of a non-lambda). Each must fail the recording —
    /// and the result must be right whether or not it did, which the twin checks. The whole-
    /// function JIT is never in play: one call each.
    #[test]
    fn traced_call_rejects_what_it_cannot_inline_and_stays_correct() {
        let mut v = boot_vm();
        let got = v.eval(
            "mk: {[n] {[x] x*n}}; cl: mk 4; m: {[a;b] a*b+1}; pj: m[;3]; G: 7; rg: {x+G}; \
             gc: {[k] s:0; i:0; while[i<k; s: s+cl i; i: i+1]; s}; \
             gcSlow: {[k] s:0; i:0; while[i<k; s: s+(- - cl i); i: i+1]; s}; \
             gp: {[k] s:0; i:0; while[i<k; s: s+neg i; i: i+1]; s}; \
             gpSlow: {[k] s:0; i:0; while[i<k; s: s+(- - neg i); i: i+1]; s}; \
             gj: {[k] s:0; i:0; while[i<k; s: s+pj i; i: i+1]; s}; \
             gjSlow: {[k] s:0; i:0; while[i<k; s: s+(- - pj i); i: i+1]; s}; \
             ga: {[k] s:0; i:0; while[i<k; p: m[i]; s: s+p 3; i: i+1]; s}; \
             gaSlow: {[k] s:0; i:0; while[i<k; p: m[i]; s: s+(- - p 3); i: i+1]; s}; \
             gg: {[k] s:0; i:0; while[i<k; s: s+rg i; i: i+1]; s}; \
             ggSlow: {[k] s:0; i:0; while[i<k; s: s+(- - rg i); i: i+1]; s}; \
             ({[k] (gc k)=gcSlow k} each 63 65 200), ({[k] (gp k)=gpSlow k} each 63 65 200), \
             ({[k] (gj k)=gjSlow k} each 63 65 200), ({[k] (ga k)=gaSlow k} each 63 65 200), \
             {[k] (gg k)=ggSlow k} each 63 65 200",
        ).unwrap();
        assert_eq!(got.fmt(), "111111111111111b");
    }

    /// A recursive callee is followed as far as the recorded iteration actually recursed — each
    /// level is one more inlined frame with one more slot range — and stopped by whichever cap
    /// it hits first: `MAX_TRACE_STEPS`/`MAX_TRACE_LOCALS` (src/trace.rs) on the recording side,
    /// or the register file (`jitTrLocalRegs`, src/neant/jit/arm64.nt) on the codegen side, as
    /// here (`rc 5` is six frames, more int locals than the file holds). Either way the loop has
    /// to stay interpreted and correct, with no hang and no stack growth beyond the interpreter's
    /// own. `big` is the same limit reached without recursion: one callee with more locals than
    /// the register file.
    #[test]
    fn traced_call_rejects_a_deep_recursion_and_too_many_locals_cleanly() {
        let mut v = boot_vm();
        let got = v.eval(
            "rc: {$[x<1; 0; x+rc[x-1]]}; \
             g: {[k] s:0; i:0; while[i<k; s: s+rc 5; i: i+1]; s}; \
             gSlow: {[k] s:0; i:0; while[i<k; s: s+(- - rc 5); i: i+1]; s}; \
             big: {a:x+1;b:a+1;c:b+1;d:c+1;e:d+1;f:e+1;g:f+1;h:g+1;a+b+c+d+e+f+g+h}; \
             gb: {[k] s:0; i:0; while[i<k; s: s+big i; i: i+1]; s}; \
             gbSlow: {[k] s:0; i:0; while[i<k; s: s+(- - big i); i: i+1]; s}; \
             ({[k] (g k)=gSlow k} each 63 64 65 200), {[k] (gb k)=gbSlow k} each 63 64 65 200",
        ).unwrap();
        assert_eq!(got.fmt(), "11111111b");
    }

    /// An int-null collision used to rewind to the loop header; now it hands the interpreter the
    /// live operand stack at the failing op (`TraceOp::Dyad`'s `resume_ip`, src/trace.rs). With
    /// `n: (i*i)+(m*m)` the colliding `m*m` has `i*i` already computed *under* it, so the exit
    /// has two values to hand back, not one — the interpreter then adds them itself and gets the
    /// null the twin gets. A handoff that dropped the stack, or rebuilt it in the wrong order,
    /// would give a wrong `n` or a stack underflow at the resumed `Dyad`. Starting `m` at `0W-200`
    /// puts the collision ~200 iterations in, well after the trace was recorded.
    #[test]
    fn traced_loop_hands_off_a_deep_live_stack_on_null_collision() {
        let mut v = boot_vm();
        let got = v.eval(
            "g: {[k] n:0; m: 0W-200; i:0; while[i<k; n: (i*i)+(m*m); m: m+1; i: i+1]; n}; \
             gS: {[k] n:0; m: 0W-200; i:0; while[i<k; n: (i*i)+(- - m*m); m: m+1; i: i+1]; n}; \
             ({[k] (g k)~gS k} each 0 1 63 64 65 200 1000), (,(g 1000)~0N)",
        ).unwrap();
        assert_eq!(got.fmt(), "11111111b");
    }

    /// A comparison result on the trace's virtual stack is tagged `bool`, not `int`, so that an
    /// exit hands it back as the `Value::Bool` the interpreter would have had. Two shapes exercise
    /// that. In `f`, `(i<k)` is computed first (a dyad's right operand is generated first) and is
    /// under the `$[..]` when its condition flips at `i=100`: the guard hands `[1b]` back, the
    /// interpreter evaluates the other branch, and `&` of two bools is a bool — an `Int` handed
    /// back instead would make it `1b & 1`, an int, and `type x` sees that. In `h` the bool is
    /// under the colliding `m+i` at an int-null handoff, and `0N + 1b` has to come out the same as
    /// in the twin. The `g` loop checks the typing rule itself while traced: `&` is a bool exactly
    /// when both operands are (`jitTrDyad`, src/neant/jit/arm64.nt), and a wrong result *type*
    /// there would be caught at recording time as a tag mismatch and leave the loop interpreted —
    /// which this can't tell from success, so `a_hot_loop_really_is_traced` is what keeps that honest.
    #[test]
    fn a_bool_live_at_a_handoff_comes_back_a_bool() {
        let mut v = boot_vm();
        let got = v.eval(
            "f: {[k] i:0; x:0; while[i<k; x: $[i<100; 1; 0b<1b] & (i<k); i: i+1]; (type x; x)}; \
             fS: {[k] i:0; x:0; while[i<k; x: $[i<100; - - 1; 0b<1b] & (i<k); i: i+1]; (type x; x)}; \
             h: {[k] s:0; m: 0W-200; i:0; while[i<k; s: (m+i)+(i<k); m: m+1; i: i+1]; s}; \
             hS: {[k] s:0; m: 0W-200; i:0; while[i<k; s: (- - m+i)+(i<k); m: m+1; i: i+1]; s}; \
             g: {[k] i:0; n:0; while[i<k; n: n+((i<50)&(i<k)); n: n+(2&(i<k)); i: i+1]; n}; \
             gS: {[k] i:0; n:0; while[i<k; n: n+((i<50)&(i<k)); n: n+(- - 2&(i<k)); i: i+1]; n}; \
             ((f 101)~(`bool;1b)), ({[k] (f k)~fS k} each 99 100 101 1000), \
             ({[k] (h k)~hS k} each 63 64 65 200 1000), ((h 1000)~0N), {[k] (g k)=gS k} each 63 64 65 200",
        ).unwrap();
        assert_eq!(got.fmt(), "111111111111111b");
    }

    /// A `do[n;..]` header is reached with its counter live on the operand stack, which used to
    /// keep it from ever being traced. Now the counter is a loop-carried stack value of the trace
    /// (`Trace::entry`, src/trace.rs) and `Op::Loop` is a step of its own: a guard that bails to
    /// the loop's exit with the counter popped, and a decrement (`jitTrLoop`, src/neant/jit/
    /// arm64.nt). Sizes straddle the 64-iteration threshold; the exit edge is what every size
    /// past it ends on, so a counter decremented wrongly, or not handed back, gives a wrong `n`.
    #[test]
    fn traced_do_loop_and_interpreted_agree() {
        let mut v = boot_vm();
        let got = v.eval(
            "d: {[k] n:0; i:0; do[k; n: n+i*i; i: i+1]; n}; \
             dS: {[k] n:0; i:0; do[k; n: n+(- - i*i); i: i+1]; n}; \
             {[k] (d k)=dS k} each 0 1 63 64 65 200 1000",
        ).unwrap();
        assert_eq!(got.fmt(), "1111111b");
    }

    /// Every way two loops can nest with a `do` involved. `do` in `do`: the inner header is hot
    /// first and is traced with *two* counters on its entry stack; the outer, when it goes hot,
    /// records the inner loop unrolled — its `Loop` steps in both directions, the exited one
    /// bailing to the `Loop` op itself if the counter is ever positive there. With an inner count
    /// of 100 the unrolled recording overruns the step cap and the outer stays interpreted while
    /// the inner runs traced from inside it. `do` in `while`: the inner `do` is entered from an
    /// interpreted `while` iteration with its counter on the stack. `while` in `do`: the inner
    /// `while` header has the outer counter under it the whole time and hands it back at every
    /// exit. A counter written back at the wrong depth, or a stack rebuilt in the wrong order,
    /// would corrupt the enclosing loop's count.
    #[test]
    fn traced_nested_do_loops_and_interpreted_agree() {
        let mut v = boot_vm();
        let got = v.eval(
            "dd: {[k] n:0; do[k; do[3; n: n+1]]; n}; \
             ddS: {[k] n:0; do[k; do[3; n: n+(- - 1)]]; n}; \
             dd2: {[k] n:0; do[k; do[100; n: n+1]]; n}; \
             dd2S: {[k] n:0; do[k; do[100; n: n+(- - 1)]]; n}; \
             dw: {[k] n:0; i:0; while[i<k; do[3; n: n+i]; i: i+1]; n}; \
             dwS: {[k] n:0; i:0; while[i<k; do[3; n: n+(- - i)]; i: i+1]; n}; \
             wd: {[k] n:0; do[k; j:0; while[j<3; n: n+j; j: j+1]]; n}; \
             wdS: {[k] n:0; do[k; j:0; while[j<3; n: n+(- - j); j: j+1]]; n}; \
             ({[k] (dd k)=ddS k} each 0 1 63 64 65 200 1000), ({[k] (dd2 k)=dd2S k} each 0 1 63 64 65 200), \
             ({[k] (dw k)=dwS k} each 0 1 63 64 65 200 1000), {[k] (wd k)=wdS k} each 0 1 63 64 65 200 1000",
        ).unwrap();
        assert_eq!(got.fmt(), "111111111111111111111111111b");
    }

    /// Every exit from a `do` trace has to hand the counter back, since the interpreter's next op
    /// expects it there. `di`: a statement-level `if` whose condition flips at `i=101` — from then
    /// on every entry bails at that guard with the counter under the popped condition, the rest
    /// of the iteration runs interpreted, and the next backward jump re-enters. `hd`: an int-null
    /// collision in the body, handed off with the counter under the null result. Either exit
    /// losing the counter would end the loop early or underflow the stack.
    #[test]
    fn traced_do_loop_exits_hand_the_counter_back() {
        let mut v = boot_vm();
        let got = v.eval(
            "di: {[k] n:0; i:0; do[k; if[i>100; n: n+10]; n: n+1; i: i+1]; n}; \
             diS: {[k] n:0; i:0; do[k; if[i>100; n: n+10]; n: n+(- - 1); i: i+1]; n}; \
             hd: {[k] s:0; m: 0W-200; i:0; do[k; s: m+i; m: m+1; i: i+1]; s}; \
             hdS: {[k] s:0; m: 0W-200; i:0; do[k; s: (- - m)+i; m: m+1; i: i+1]; s}; \
             ({[k] (di k)=diS k} each 0 1 63 64 65 100 101 102 1000), ({[k] (hd k)~hdS k} each 63 64 65 200 1000), (,(hd 1000)~0N)",
        ).unwrap();
        assert_eq!(got.fmt(), "111111111111111b");
    }

    /// A call in a `do` body inlines exactly as one in a `while` body does — the counter sits
    /// under the callee's frame on the virtual stack. `ab` has a branch inside it that flips at
    /// `i=101`, so from then on every entry *rewinds*: the only exit that can't hand the stack
    /// off, since its ip is in the callee's frame. A `do` trace with a rewind in it stores its
    /// entry stack (the counter) at the top of every iteration alongside its written locals, and
    /// the rewind stub hands that back with the header ip — a counter missing there would restart
    /// the loop with a stale or empty stack.
    #[test]
    fn traced_do_loop_with_an_inlined_call_and_interpreted_agree() {
        let mut v = boot_vm();
        let got = v.eval(
            "f: {x*2}; ab: {$[x<0;0-x;x]}; \
             dc: {[k] s:0; i:0; do[k; s: s+f i; i: i+1]; s}; \
             dcS: {[k] s:0; i:0; do[k; s: s+(- - f i); i: i+1]; s}; \
             dr: {[k] s:0; i:0; do[k; s: s+ab[100-i]; i: i+1]; s}; \
             drS: {[k] s:0; i:0; do[k; s: s+(- - ab[100-i]); i: i+1]; s}; \
             ({[k] (dc k)=dcS k} each 0 1 63 64 65 200 1000), {[k] (dr k)=drS k} each 63 64 65 100 101 102 1000",
        ).unwrap();
        assert_eq!(got.fmt(), "11111111111111b");
    }

    /// Two shapes the recorder refuses, which must then simply stay interpreted and right. An
    /// `Op::Loop` inside an inlined callee (`cnt`) would need a bail into the callee's bytecode,
    /// and unlike a callee's branch it can't be rewound either — it mutates the stack — so the
    /// recording fails and the calling loop is never traced. A `do` whose counter isn't a plain
    /// int is legal for the interpreter (`int_of` takes a bool or a whole float) but not for a
    /// trace: as the traced loop's own header (`fl`) the entry check refuses to record; nested
    /// inside a traced `while` (`nb`, `nf`) the counter carries a `bool`/`float` tag on the
    /// virtual stack and `jitTrLoop` rejects it at codegen.
    #[test]
    fn a_do_loop_the_trace_cannot_take_stays_interpreted_and_correct() {
        let mut v = boot_vm();
        let got = v.eval(
            "cnt: {[x] n:0; do[x; n: n+1]; n}; \
             g: {[k] s:0; i:0; while[i<k; s: s+cnt 3; i: i+1]; s}; \
             gS: {[k] s:0; i:0; while[i<k; s: s+(- - cnt 3); i: i+1]; s}; \
             fl: {[k] n:0; do[k*1.0; n: n+1]; n}; \
             flS: {[k] n:0; do[k*1.0; n: n+(- - 1)]; n}; \
             nb: {[k] n:0; i:0; while[i<k; do[1b; n: n+1]; i: i+1]; n}; \
             nbS: {[k] n:0; i:0; while[i<k; do[1b; n: n+(- - 1)]; i: i+1]; n}; \
             nf: {[k] n:0; i:0; while[i<k; do[2.0; n: n+1]; i: i+1]; n}; \
             nfS: {[k] n:0; i:0; while[i<k; do[2.0; n: n+(- - 1)]; i: i+1]; n}; \
             ({[k] (g k)=gS k} each 63 64 65 200), ({[k] (fl k)=flS k} each 63 64 65 200), \
             ({[k] (nb k)=nbS k} each 63 64 65 200), {[k] (nf k)=nfS k} each 63 64 65 200",
        ).unwrap();
        assert_eq!(got.fmt(), "1111111111111111b");
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

    /// A `do` loop against its `- -` twin, the same way `manual_trace_perf_measurement` measures
    /// the `while` form. The traced body is one `Op::Loop` step (a compare, a branch, a
    /// decrement) longer than the `while` loop's is shorter (no `i<k`), so the two should land
    /// within noise of each other.
    #[test]
    #[ignore]
    fn manual_trace_do_perf_measurement() {
        let mut v = boot_vm();
        v.eval("d: {[k] n:0; i:0; do[k; n: n+i*i; i: i+1]; n}").unwrap();
        v.eval("dSlow: {[k] n:0; i:0; do[k; n: n+(- - i*i); i: i+1]; n}").unwrap();
        let t0 = std::time::Instant::now();
        let slow = v.eval("dSlow 5000000").unwrap();
        let interpreted = t0.elapsed();
        let t1 = std::time::Instant::now();
        let fast = v.eval("d 5000000").unwrap();
        let traced = t1.elapsed();
        assert_eq!(slow.fmt(), fast.fmt());
        println!("interpreted {interpreted:?}  traced {traced:?}  ratio {:.1}x", interpreted.as_secs_f64() / traced.as_secs_f64());
    }

    /// The calling loop against the same loop with `- -` spliced in — the untraceable twin, which
    /// is the interpreted baseline here since a `do` loop can't call a function per iteration
    /// any more cheaply than a `while` one can. One call each, so the whole-function JIT (64
    /// calls) never enters; what this measures is a trace with an inlined call against the
    /// interpreter's own `Op::Call` → `call_code` → `execute` per iteration.
    #[test]
    #[ignore]
    fn manual_trace_call_perf_measurement() {
        let mut v = boot_vm();
        v.eval("f: {x*2}").unwrap();
        v.eval("g: {[k] s:0; i:0; while[i<k; s: s+f i; i: i+1]; s}").unwrap();
        v.eval("gSlow: {[k] s:0; i:0; while[i<k; s: s+(- - f i); i: i+1]; s}").unwrap();
        let t0 = std::time::Instant::now();
        let slow = v.eval("gSlow 5000000").unwrap();
        let interpreted = t0.elapsed();
        let t1 = std::time::Instant::now();
        let fast = v.eval("g 5000000").unwrap();
        let traced = t1.elapsed();
        assert_eq!(slow.fmt(), fast.fmt());
        println!("interpreted {interpreted:?}  traced {traced:?}  ratio {:.1}x", interpreted.as_secs_f64() / traced.as_secs_f64());
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
    /// `badd` is what makes this possible: a word may be any bit pattern, including the one `+` reads as 0N.
    #[test]
    fn sha512_matches_fips_vectors() {
        let mut v = ed_vm();
        for (src, want) in [
            ("hex sha512 0x",
             "\"cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e\""),
            ("hex sha512 `byte$\"abc\"",
             "\"ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f\""),
            // two blocks, and a length that forces a padding block of its own
            ("hex sha512 `byte$\"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu\"",
             "\"8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909\""),
            ("hex sha512 `byte$ 200#\"a\"",
             "\"4b11459c33f52a22ee8236782714c150a3b2c60994e9acee17fe68947a3e6789f31e7668394592da7bef827cddca88c4e6f86e4df7ed1ae6cba71f3e98faee9f\""),
            ("mn: 1 shl 63; (mn badd 1; mn + 1)", "-0W 0N"),   // why `badd` has to exist
        ] {
            assert_eq!(tests::ev(&mut v, src), want, "source: {src}");
        }
    }
    /// The group law, independent of any signature: the neutral element, negation, and doubling
    /// two ways. The addition formula is the complete one, so `edAdd` must handle the identity.
    #[test]
    fn group_law_holds() {
        let mut v = ed_vm();
        for (src, want) in [
            ("(edEncode EDB) ~ fencode EDBY", "1b"),
            ("(edEncode edDecode edEncode EDB) ~ edEncode EDB", "1b"),
            ("(edEncode edAdd[EDB;EDB]) ~ edEncode edDbl EDB", "1b"),
            ("(edEncode edAdd[EDZERO;EDB]) ~ edEncode EDB", "1b"),
            ("(edEncode edAdd[EDB;edNeg EDB]) ~ edEncode EDZERO", "1b"),
            ("(edEncode edMul[253#scBits scFromBytes 0x02,31#0x00; EDB]) ~ edEncode edDbl EDB", "1b"),
            ("scMod bitsBE unhex \"1000000000000000000000000000000014def9dea2f79cd65812631a5cf5d3ed\"", "0 0 0 0 0 0 0 0 0 0 0 0"),
            ("scMod bitsBE (31#0x00),0x01", "1 0 0 0 0 0 0 0 0 0 0 0"),
        ] {
            assert_eq!(tests::ev(&mut v, src), want, "source: {src}");
        }
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

