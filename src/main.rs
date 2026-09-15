//! neant: a vector language. Stage 0 — compiler and bytecode VM in Rust.
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
//! Stage 1: port lex/parse/compile to neant itself and run them on this VM.
mod compile;
mod image;
mod lex;
mod parse;
mod prims;
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
    let flag = argv.get(1).map(String::as_str);
    if flag == Some("--build-boot") {   // stage 0: Rust front end loads boot/*.nt, the neant compiler compiles them into the image
        let mut vm = stage0(&read);
        let bytes = build_boot_image(&mut vm, &read).unwrap_or_else(|e| { eprintln!("'{}", e.0); std::process::exit(1) });
        std::fs::write(BOOT_IMAGE_PATH, &bytes).unwrap_or_else(|e| { eprintln!("{BOOT_IMAGE_PATH}: {e}"); std::process::exit(2) });
        println!("wrote {BOOT_IMAGE_PATH} ({} bytes); rebuild to embed it", bytes.len());
        return;
    }
    let rust_fe = flag == Some("--rust");   // debugging aid: the Rust lexer/parser/compiler instead of the boot image
    let mut vm = if rust_fe { prelude_vm() } else { boot_vm() };
    let file = argv.get(1 + rust_fe as usize);
    vm.set("args", value::list(argv.iter().skip(2 + rust_fe as usize).map(|a| value::chars(a.chars().collect())).collect()));
    let nrun = vm.get("nrun");
    let run = |vm: &mut vm::Vm, src: &str| match &nrun {
        Some(f) => vm.call(f, vec![value::chars(src.chars().collect())]),
        None => vm.run(src),
    };
    if let Some(path) = file {
        if !report(run(&mut vm, &read(path))) { std::process::exit(1); }
        return;
    }
    let stdin = std::io::stdin();
    let mut line = String::new();
    loop {
        print!("neant) "); std::io::stdout().flush().ok();
        line.clear();
        if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 { println!(); return; }
        report(run(&mut vm, &line));
    }
}

const BOOT_FILES: [&str; 7] = ["boot/prelude.nt", "boot/lex.nt", "boot/parse.nt", "boot/compile.nt", "boot/table.nt", "boot/json.nt", "boot/crypto.nt"];
/// Standard library written in neant; the Rust front end needs it loaded too (`--rust`, tests).
const PRELUDE: &str = include_str!("../boot/prelude.nt");
fn prelude_vm() -> vm::Vm {
    let mut vm = vm::Vm::new();
    vm.run(PRELUDE).unwrap_or_else(|e| { eprintln!("prelude: '{}", e.0); std::process::exit(2) });
    vm
}
const BOOT_IMAGE_PATH: &str = "boot/boot.nb";
/// The self-hosted front end, compiled by itself: a list of bytecode units defining nlex/nparse/ncompile/nrun.
const BOOT_IMAGE: &[u8] = include_bytes!("../boot/boot.nb");

/// A VM with the embedded boot image loaded — the normal way to run neant code.
fn boot_vm() -> vm::Vm {
    let mut vm = vm::Vm::new();
    let img = image::load(BOOT_IMAGE).and_then(|img| vm.exec(&img));
    if let Err(e) = img { eprintln!("boot image: '{}  (run `neant --build-boot`, then rebuild)", e.0); std::process::exit(2); }
    vm
}
/// A VM with boot/*.nt loaded through the Rust front end (only for building the image and for tests).
fn stage0(read: &dyn Fn(&str) -> String) -> vm::Vm {
    let mut vm = vm::Vm::new();
    for f in BOOT_FILES { vm.run(&read(f)).unwrap_or_else(|e| { eprintln!("{f}: '{}", e.0); std::process::exit(2) }); }
    vm
}
fn build_boot_image(vm: &mut vm::Vm, read: &dyn Fn(&str) -> String) -> value::R<Vec<u8>> {
    let mut units = Vec::new();
    for f in BOOT_FILES {
        vm.set("src", value::chars(read(f).chars().collect()));
        units.extend(vm.run("ncompile src")?.seq());
    }
    image::dump(&value::pack(units))
}

#[cfg(test)]
mod tests {
    use super::*;
    pub fn table_vm() -> vm::Vm {
        let mut v = prelude_vm();
        for f in ["boot/table.nt", "boot/json.nt", "boot/crypto.nt"] { v.run(&std::fs::read_to_string(f).unwrap()).unwrap(); }
        v
    }
    fn ev(src: &str) -> String { table_vm().run(src).map(|v| v.fmt()).unwrap_or_else(|e| format!("'{}", e.0)) }

    pub const CASES: &[(&str, &str)] = &[
            ("1+2", "3"),
            ("2*3+4", "14"),                        // right-to-left, no precedence
            ("1 2 3+10", "11 12 13"), ("1 2 3+10 20 30", "11 22 33"), ("1.5+1", "2.5"),
            ("1 - 2", "-1"), ("1 -2", "1 -2"),      // space rule for negative literals
            ("+/1 2 3 4", "10"), ("+\\1 2 3", "1 3 6"), ("*/1 2 3 4", "24"), ("|/3 9 2", "9"), ("+/1.5 2.5", "4f"),
            ("#1 2 3", "3"), ("!5", "0 1 2 3 4"), ("3#1 2", "1 2 1"), ("-2#1 2 3", "2 3"), ("1_1 2 3", "2 3"), ("-1_1 2 3", "1 2"),
            ("1 2 3,4 5", "1 2 3 4 5"), (",1", ",1"), ("|1 2 3", "3 2 1"), ("&0 1 0 2", "1 3 3"),
            ("<3 1 2", "1 2 0"), (">3 1 2", "0 2 1"), ("?1 1 2 3 3", "1 2 3"), ("1 2 3?2", "1"), ("1 2 3?9", "3"), ("1 2 3@0 2", "1 3"),
            ("1 2 3=1 5 3", "101b"), ("1 2~1 2", "1b"), ("1~1.0", "0b"), ("~0 1", "10b"),
            ("6%3", "2f"), ("2^10", "1024"), ("2^0.5", "1.4142135623730951"), ("_2.7", "2"), ("-1 2", "-1 2"), ("- 1 2", "-1 -2"),
            ("x:1 2 3;x*x", "1 4 9"),
            ("f:{x*y};f[3;4]", "12"), ("{x+1} 5", "6"), ("{[a;b] a-b}[10;3]", "7"), ("{z} [1;2;3]", "3"),
            ("$[1<2;`yes;`no]", "`yes"), ("$[0;1;0;2;3]", "3"), ("$[0;1]", "::"),
            ("fact:{$[x<2;1;x*fact x-1]};fact 10", "3628800"),
            ("n:0;i:0;while[i<5;n:n+i;i:i+1];n", "10"),
            ("if[1;r:5];r", "5"),
            ("g:{a:x*2;a+1};g 3", "7"),            // locals stay local
            ("{x+y}'[1 2 3;10 20 30]", "11 22 33"), ("{x*x}'1 2 3", "1 4 9"), ("{x,y}'[1;2 3]", "(1 2;1 3)"),
            ("0 {x+y}/1 2 3", "6"), ("{x+y}/1 2 3", "6"), ("{x*y}\\1 2 3 4", "1 2 6 24"),
            ("\"ab\",\"cd\"", "\"abcd\""), ("#\"hello\"", "5"), ("`a`b", "`a`b"), ("`a", "`a"), ("\"a\"", "\"a\""),
            ("(1;`a;\"s\")", "(1;`a;\"s\")"), ("(1;2)", "1 2"), ("(1 2;3 4)", "(1 2;3 4)"), ("+(1 2;3 4)", "(1 3;2 4)"),
            ("d:`a`b!1 2;d`b", "2"), ("d:`a`b!1 2;d`b`a", "2 1"), ("key `a`b!1 2", "`a`b"), ("value `a`b!1 2", "1 2"),
            ("avg 1 2 3 4", "2.5"), ("sum 1 2 3", "6"), ("7 mod 3", "1"), ("7 div 2", "3"), ("max 3 1 2", "3"),
            ("1 2 3 in 2 3 4", "011b"), ("5 within 1 10", "1b"), ("1 2 3 within 2 3", "011b"), ("1=1 1 0", "110b"), ("(1=1 0)&1=0 1", "00b"),
            // group, each over dicts, tables (boot/table.nt)
            ("=1 2 1 3 1", "1 2 3!(0 2 4;,1;,3)"), ("{x*2} each `a`b!1 2", "`a`b!2 4"), ("\"\" ~ \"abc\"[()]", "1b"),
            ("t:tbl[`a`b;(1 2 3;10 20 30)];t`b", "10 20 30"), ("t:tbl[`a`b;(1 2 3;10 20 30)];row[t;1]", "`a`b!2 20"), ("t:tbl[`a`b;(1 2 3;10 20 30)];tcount t", "3"),
            ("t:tbl[`a`b;(1 2 3;10 20 30)];tsel[t;t[`a]>1]", "`a`b!(2 3;20 30)"), ("t:tbl[`a`b;(3 1 2;10 20 30)];tsort[t;`a]`b", "20 30 10"),
            ("t:tbl[`k`v;(`x`y`x;1 2 3)];tby[t;`k;`v;sum]", "`x`y!4 2"), ("t:tbl[`a`b;(1 2;3 4)];tcount tappend[t;t]", "4"),
            // literals: bools, nulls, dates, times, namespaces
            ("101b", "101b"), ("1b", "1b"), ("0N 1 0W", "0N 1 0W"), ("0N+1", "0N"), ("0n 1.5", "0n 1.5"), ("-0W", "-0W"),
            ("2026.09.15", "2026.09.15"), ("2026.09.15+30", "2026.10.15"), ("2026.12.31-2026.01.01", "364"), ("`year`month`day$\\:2026.09.15", "2026 9 15"),
            ("12:30:00", "12:30:00.000"), ("`hour$12:30:00.250+1000", "12"), ("`date$\"2026.02.28\"", "2026.02.28"), ("`date$\"2026.02.30\"", "'parse: not a date: \"2026.02.30\""),
            ("isnull 0N 1", "10b"), ("fill[0;1 0N 3]", "1 0 3"), ("fills 1 0N 0N 4", "1 1 1 4"), (".ns.v: 7;.ns.v", "7"), ("2026.01.01<2026.01.02", "1b"),
            // closures (capture by value), do, deep and compound index assignment, each-left/right
            ("f:{n:10;{x+n}};g:f 0;g 5", "15"), ("add:{[a] {[b] a+b}};(add 3) 4", "7"), ("h:{a:1;b:{c:2;{a+c+x}};b[][10]};h 0", "13"),
            ("f:{n:1;g:{n};n:2;g[]}; f 0", "1"),
            ("x:(1 2;3 4);x[1;0]:9;x", "(1 2;9 4)"), ("c:1 2 3;c[1]+:10;c", "1 12 3"), ("n:0;do[5;n+:2];n", "10"), ("n:0;while[1;n+:1;if[n>4;break]];n", "5"), ("n:0;do[10;n+:1;if[n=3;break]];n", "3"), ("{break} 0", "'compile: break outside a loop"), ("2026.01.01 2026.01.03", "2026.01.01 2026.01.03"), ("1+2026.01.01 2026.01.03", "2026.01.02 2026.01.04"),
            ("1 2 3 +\\: 10 20", "(11 21;12 22;13 23)"), ("1 2 3 +/: 10 20", "(11 12 13;21 22 23)"), ("{x,y}\\:[1 2;3]", "(1 3;2 3)"),
            // prelude additions
            ("deltas 1 3 6", "1 2 3"), ("sums 1 2 3", "1 3 6"), ("prev 1 2 3", "0N 1 2"), ("next 1 2 3", "2 3 0N"), ("1 2 3 4 except 2 4", "1 3"),
            ("cross[1 2;`a`b]", "((1;`a);(1;`b);(2;`a);(2;`b))"), ("fmt[\"% + % = %\";(1;2;3)]", "\"1 + 2 = 3\""), ("asc 3 1 2", "1 2 3"), ("desc 3 1 2", "3 2 1"), ("asc (\"pear\";\"apple\";\"fig\")", "(\"apple\";\"fig\";\"pear\")"), ("<(2 1;1 9;1 2)", "2 1 0"),
            ("\"hello.nt\" like \"*.nt\"", "1b"), ("\"hello\" like \"h?l*\"", "1b"), ("\"hello\" like \"h?x*\"", "0b"), ("ssr[\"a-b-c\";\"-\";\"+\"]", "\"a+b+c\""),
            ("t:tbl[`a`b;(2 1 1;5 9 2)];xasc[`a`b;t]`b", "2 9 5"), ("t:tbl[`a`b;(2 1 1;5 9 2)];xdesc[`b;t]`a", "1 2 1"),
            ("x:1\n\ny+1", "'undefined: y at line 3"), ("f:{x+`a}\nf 1", "'type: arithmetic on non-numeric at line 2"),
            // select, joins
            ("t:tbl[`k`v;(`x`y`x;1 2 3)];select sum v by k from t", "`k`v!(`x`y;4 2)"),
            ("t:tbl[`a`b;(1 2 3;10 20 30)];select total: sum b, n: count a from t", "`total`n!(,60;,3)"),
            ("t:tbl[`a`b;(1 2 3;10 20 30)];select from t where a>1, b<30", "`a`b!(,2;,20)"),
            ("t:tbl[`a`b;(1 2 3;10 20 30)];m:15;select b, c: a*2 from t where b>m", "`b`c!(20 30;4 6)"),
            ("t:tbl[`k`v;(`a`b`c;1 2 3)];u:tbl[`k`w;(`a`c;10 30)];lj[t;`k;u]", "`k`v`w!(`a`b`c;1 2 3;10 0N 30)"),
            ("t:tbl[`k`v;(`a`b`c;1 2 3)];u:tbl[`k`w;(`a`c;10 30)];ij[t;`k;u]", "`k`v`w!(`a`c;1 3;10 30)"),
            ("t:tbl[`a;,1 2];u:tbl[`b;,`x];uj[t;u]", "`a`b!(1 2 0N;```x)"),
            ("ungroup tbl[`a`b;(1 2;((10 11);,20))]", "`a`b!(1 1 2;10 11 20)"),
            ("t:tbl[`a`b;(1 2 3;10 20 30)];t[1]", "`a`b!2 20"), ("t:tbl[`a`b;(1 2 3;10 20 30)];t[0 2]", "`a`b!(1 3;10 30)"), ("t:tbl[`a`b;(1 2 3;10 20 30)];t[where t[`b]>15]`a", "2 3"),
            ("kt:xkey[`id;tbl[`id`v;(7 8 9;`a`b`c)]];kt[8]", ",`v!,`b"), ("kt:xkey[`id;tbl[`id`v;(7 8 9;`a`b`c)]];key kt", "7 8 9"), ("kt:xkey[`id;tbl[`id`v;(7 8 9;`a`b`c)]];unkey kt", "`k`v!(7 8 9;`a`b`c)"), ("kt:xkey[`a`b;tbl[`a`b`v;(1 1 2;1 2 1;10 20 30)]];kt[(1;2)]", ",`v!,20"),
            ("distinct (1;`a;1;\"s\";`a)", "(1;`a;\"s\")"), ("distinct 0n 1 0n", "0n 1f"), ("group 2026.01.01 2026.01.02 2026.01.01", "2026.01.01 2026.01.02!(0 2;,1)"), ("(1 2;3 4)?3 4", "1"),
            // prelude: math, windows, lists, strings, random
            ("neg 1 2", "-1 -2"), ("ceiling 1.5 2", "2 2"), ("round 1.4 1.6", "1 2"), ("signum[-3 0 2]", "-1 0 1"), ("med 3 1 2 10", "2.5"), ("med 3 1 2", "2"),
            ("var 1 2 3 4", "1.25"), ("dev 2 4 4 4 5 5 7 9", "2f"), ("rank 30 10 20", "2 0 1"), ("10 xbar 12 27 30", "10 20 30"), ("7 xbar 2026.01.05", "2026.01.03"),
            ("1 3 5 bin 4", "1"), ("1 3 5 bin 0 3 9", "-1 1 2"), ("any 0=1 1 0", "1b"), ("all 0=1 1 0", "0b"),
            ("2 msum 1 2 3 4", "1 3 5 7"), ("2 mavg 1 2 3 4", "1f 1.5 2.5 3.5"), ("2 mmax 1 3 2 5 4", "1 3 3 5 5"), ("2 mmin 3 1 2", "3 1 1"), ("0.5 ema 1 2 3", "1f 1.5 2.25"),
            ("1 rotate 1 2 3", "2 3 1"), ("-1 rotate 1 2 3", "3 1 2"), ("2 cut til 5", "(0 1;2 3;,4)"), ("0 3 cut til 5", "(0 1 2;3 4)"), ("2 sublist 1 2 3", "1 2"), ("1 5 sublist 1 2 3", "2 3"), ("differ 1 1 2 2 3", "10101b"),
            ("ltrim \"  a \"", "\"a \""), ("rtrim \"  a \"", "\"  a\""), ("ssr[\"a-b-c\";\"-\";\"+\"]", "\"a+b+c\""), ("\"aaaa\" ss \"aa\"", "0 2"),
            ("\"hello\" like \"h*o\"", "1b"), ("\"hello\" like \"h?l*\"", "1b"), ("\"hello\" like \"x*\"", "0b"), ("\"hello.nt\" like \"*.nt\"", "1b"),
            ("rseed 7; count 5 rand 10", "5"), ("rseed 7; all (5 rand 10) < 10", "1b"), ("rseed 7; x: 3 rand 10; rseed 7; x ~ 3 rand 10", "1b"), ("rseed 1; @rand 1.0", "`float"), ("rseed 1; (rand `a`b) in `a`b", "1b"),
            ("sin 0", "0f"), ("cos 0", "1f"), ("atan 1", "0.7853981633974483"),
            // asof join, JSON (boot/json.nt)
            ("t:tbl[`s`t`v;(`a`b`a;1 5 9;10 20 30)];q:tbl[`s`t`p;(`a`a`b;0 8 4;1 2 3)];aj[`s`t;t;q]`p", "1 3 2"), ("t:tbl[`t`v;(1 5 9;10 20 30)];q:tbl[`t`p;(0 8;1 2)];aj[`t;t;q]`p", "1 1 2"),
            ("jk \"{\\\"a\\\": [1, 2.5, \\\"s\\\"], \\\"b\\\": true, \\\"c\\\": null}\"", "`a`b`c!((1f;2.5;\"s\");1b;::)"), ("jk \"[1,2,3]\"", "1 2 3"), ("jk \"[]\"", "()"), ("jk \"{}\"", "()!()"), ("jk \" -1.5e2 \"", "-150f"),
            ("count jk \"\\\"a\\\\nb\\\"\"", "3"), ("jk \"\\\"\\\\u00e9\\\"\"", "\"é\""), ("jk \"{\\\"a\\\":{\\\"b\\\":[{\\\"c\\\":1}]}}\"", ",`a!(,`b!((,`c!,1)))"),
            ("jj `a`b`c!(1 2;\"x\";null)", "\"{\"a\": [1, 2], \"b\": \"x\", \"c\": null}\""), ("jj (1b;0b;null;1.5;`s;4.0)", "\"[true, false, null, 1.5, \"s\", 4]\""), ("jj \"q\\\"\\\\\"", "\"\"q\\\"\\\\\"\""),
            ("jk jj `a`b`c!(1 2;\"x\";null)", "`a`b`c!(1 2;\"x\";::)"), ("jj tbl[`a`b;(1 2;`x`y)]", "\"{\"a\": [1, 2], \"b\": [\"x\", \"y\"]}\""),
            ("jk \"[1,\"", "'json: unexpected end at 3"), ("jk \"[1 2]\"", "'json: expected , or ] at 4"), ("jk \"tru\"", "'json: bad literal at 0"), ("jk \"{\\\"a\\\":1,}\"", "'json: expected key at 7"), ("jj {x}", "'json: cannot serialize a function"),
            // bytes, bit verbs, SHA-256 (boot/crypto.nt)
            ("0x0aff", "0x0aff"), ("type 0x0a", "`byte"), ("0x0aff[1]", "0xff"), ("x: 0x; x,: 0x01; x,: 0x0203; x", "0x010203"), ("`int$0x0aff", "10 255"), ("`byte$255 256 65", "0xff0041"),
            ("`byte$\"hé\"", "0x68c3a9"), ("`char$0x68c3a9", "\"hé\""), ("hex 0x0aff", "\"0aff\""), ("unhex \"0aff\"", "0x0aff"), ("0x0a+1", "11"),
            ("0x0aff bxor 0xff00", "0xf5ff"), ("0x0f band 0x3c", "0x0c"), ("bnot 0x0f", "0xf0"), ("12 bxor 10", "6"), ("255 shl 8", "65280"), ("-1 shr 60", "15"), ("1 2 3 shl 1 2 3", "2 8 24"), ("rotr32[1;1]", "2147483648"),
            ("hex sha256 0x", "\"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\""),
            ("hex sha256 `byte$\"abc\"", "\"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\""),
            ("hex sha256 `byte$\"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq\"", "\"248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1\""),
            // ChaCha20, HMAC/HKDF, Poly1305, AEAD, X25519 (boot/crypto.nt): RFC 8439 / 4231 / 5869 / 7748 vectors
            ("k: `byte$til 32; hex chachaBlock[k;1;0x000000090000004a00000000]", "\"10f1e7e4d13b5915500fdd1fa32071c4c7d1f4c733c068030422aa9ac3d46c4ed2826446079faa0914c2d705d98b02a2b5129cd1de164eb9cbd083e8a2503c4e\""),
            ("k: `byte$til 32; hex 16#chacha20[k;1;0x000000000000004a00000000;`byte$\"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.\"]", "\"6e2e359a2568f98041ba0728dd0d6981\""),
            ("k: `byte$til 32; d: `byte$\"round trip\"; `char$chacha20[k;7;12#0x01;chacha20[k;7;12#0x01;d]]", "\"round trip\""),
            ("hex hmac[20#0x0b;`byte$\"Hi There\"]", "\"b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7\""),
            ("hex hkdfExtract[unhex \"000102030405060708090a0b0c\";22#0x0b]", "\"077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5\""),
            ("hex hkdfExpand[hkdfExtract[unhex \"000102030405060708090a0b0c\";22#0x0b];unhex \"f0f1f2f3f4f5f6f7f8f9\";42]", "\"3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865\""),
            ("hex poly1305[unhex \"85d6be7857556d337f4452fe42d506a80103808afb0db2fd4abff6af4149f51b\";`byte$\"Cryptographic Forum Research Group\"]", "\"a8061dc1305136c6c22b8baf0c0127a9\""),
            ("r: aeadEncrypt[`byte$128+til 32;unhex \"070000004041424344454647\";unhex \"50515253c0c1c2c3c4c5c6c7\";`byte$\"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.\"]; (hex 16#r[0]; hex r[1])", "(\"d31a8d34648e60db7b86afbc53ef7ec2\";\"1ae10b594f09e26a7e902ecbd0600691\")"),
            ("k: `byte$til 32; r: aeadEncrypt[k;12#0x02;0xaa;`byte$\"hi\"]; `char$aeadDecrypt[k;12#0x02;0xaa;r[0];r[1]]", "\"hi\""),
            ("k: `byte$til 32; r: aeadEncrypt[k;12#0x02;0xaa;`byte$\"hi\"]; aeadDecrypt[k;12#0x02;0xaa;r[0];bnot r[1]]", "'aead: bad tag"),
            ("hex fencode fadd[fsub[F0;F1];F1]", "\"0000000000000000000000000000000000000000000000000000000000000000\""), ("hex fencode fmul[fdecode 0x07,31#0x00;finv fdecode 0x07,31#0x00]", "\"0100000000000000000000000000000000000000000000000000000000000000\""),
            ("hex x25519[unhex \"77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a\";X25519BASE]", "\"8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a\""),
            ("hex x25519[unhex \"5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb\";unhex \"8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a\"]", "\"4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742\""),
            // error positions
            ("(1", "'parse: missing ) at line 1"), ("1+2\n(1", "'parse: missing ) at line 2"), ("\"ab\n\nc", "'lex: unterminated string at line 1"),
            ("// comment\n5", "5"), ("1+2\n3+4", "7"),
            ("f:+;f[1;2]", "3"), ("f:+/;f 1 2 3", "6"), ("(+/)1 2 3", "6"),  // verbs and adverbed verbs are values
            ("+/{x*x} til 1000", "332833500"),
            ("raze (1 2;3 4)", "1 2 3 4"), ("sort 3 1 2", "1 2 3"), ("string 42", "\"42\""), ("`$\"ab\"", "`ab"), ("type 1 2", "`ints"),
            // index assignment, compound assignment
            ("x:1 2 3;x[1]:9;x", "1 9 3"), ("x:1 2 3;x[0 2]:7;x", "7 2 7"), ("x:1 2 3;x[1]:`a;x", "(1;`a;3)"),
            ("x:1 2 3;x[5]:1", "'index: amend out of range"), ("x:1 2 3;@[{x[5]:1};0;0];x", "1 2 3"),
            ("d:`a`b!1 2;d[`c]:3;d", "`a`b`c!1 2 3"), ("d:`a`b!1 2;d[`a]:9;d`a", "9"),
            ("n:1;n+:2;n", "3"), ("x:();x,:1;x,:2 3;x", "1 2 3"), ("f:{a:();a,:x;a,:,x;a};f 1 2", "(1;2;1 2)"), ("g::();{g::g,x} each 1 2;g", "1 2"), ("s:\"\";s,:\"a\";s,:\"bc\";s", "\"abc\""), ("x:1 2;y:x;x,:3;y", "1 2"), ("g::1;{g::x} 5;g", "5"), ("g:1;{g:x} 5;g", "1"), ("x:1 2;x,:3;x", "1 2 3"),
            ("f:{x[0]:9;x};y:1 2;f y;y", "1 2"), ("f:{x[0]:9;x};f 1 2", "9 2"),   // value semantics
            ("f:{[x] a:1 2 3;a[0]:9;a};f 0", "9 2 3"),
            // projection
            ("f:{[a;b] a-b};g:f[10;];g 3", "7"), ("f:{[a;b] a-b};f[;3] 10", "7"), ("{x+y}[1] 2", "3"), ("{5}[]", "5"), ("{5} 0", "5"), ("{if[x<0; :`neg]; `pos}[-1]", "`neg"), ("{if[x<0; :`neg]; `pos} 1", "`pos"), ("|/0=1 1 0", "1b"), ("&/1=1 1 0", "0b"), ("-[;1] 5", "4"),
            ("type {x+y}[1]", "`fn"), ("{x+y}[1]", "{[x;y]...}[1;]"), ("f:{[a;b;c] a,b,c};f[1][2][3]", "1 2 3"),
            // adverb keywords, protected evaluation
            ("{x*2} each 1 2 3", "2 4 6"), ("(+) over 1 2 3", "6"), ("{x+y} scan 1 2 3", "1 3 6"),
            ("@[{x+1};1;{\"caught\"}]", "2"), ("@[{signal \"boom\"};1;{x}]", "\"boom\""), ("@[{1+`a};0;\"fallback\"]", "\"fallback\""),
            ("@[{x+`a};1;{\"err: \",x}]", "\"err: type: arithmetic on non-numeric\""),
            // strings, casts
            ("\",\" vs \"a,b,c\"", "(\"a\";\"b\";\"c\")"), ("\",\" sv (\"ab\";\"cd\")", "\"ab,cd\""), ("\"hello\" ss \"l\"", "2 3"),
            ("upper \"ab\"", "\"AB\""), ("trim \"  a \"", "\"a\""), ("\"\\n\" vs \"a\\nb\"", "(\"a\";\"b\")"),
            ("`int$\"42\"", "42"), ("`float$\"1.5\"", "1.5"), ("`int$3.7", "3"), ("`char$65 66", "\"AB\""), ("`code$\"A\"", "65"),
            ("`int$(\"1\";\"22\")", "1 22"), ("`int$\"x\"", "'parse: not an int: \"x\""),
            ("\"abc\"[1]", "\"b\""), ("\"abc\"~\"abc\"", "1b"), ("\"abc\"=\"abd\"", "110b"),
            ("undefined_name", "'undefined: undefined_name"), ("1 2+1 2 3", "'length"), ("{x}[1;2]", "'rank: expected 1 args, got 2"),
        ];

    #[test]
    fn language() {
        for (src, want) in CASES {
            assert_eq!(ev(src), *want, "source: {src}");
        }
    }

    #[test]
    fn locals_do_not_leak() {
        let mut v = vm::Vm::new();
        v.run("g:{a:x*2;a+1};g 3").unwrap();
        assert!(v.get("a").is_none());
    }

    #[test]
    fn fused_fold_is_fast() {
        let t = std::time::Instant::now();
        let v = prelude_vm().run("+/{x*x} til 2000000").unwrap();
        assert_eq!(v.fmt(), "2666664666667000000");
        assert!(t.elapsed().as_millis() < 500, "took {:?}", t.elapsed());
    }
}

#[cfg(test)]
mod boot {
    use super::*;
    /// The self-hosted lexer (boot/lex.nt) must produce exactly the Rust lexer's token stream.
    #[test]
    fn self_hosted_lexer_matches_oracle() {
        let mut v = prelude_vm();
        v.run(&std::fs::read_to_string("boot/lex.nt").unwrap()).unwrap();
        let corpus = [
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
            "101b 1b 0N 0W -0W 0n 0w 1 0N 3 2.5 0n", "2026.09.15 12:30:00 12:30:00.25 9:05 2026.13.01 12:30", "2026.01.01 2026.01.03 12:00:00 2026.01.01 5", ".ns.v .z.d x.y 1.5.2",
            "x +\\: y; x +/: y; f\\:[a;b]; \"a\\nb\" \\ 5", "0x0aff 0x0a 0x; x: 0x01,0xAB",
        ];
        for src in corpus {
            v.set("src", value::chars(src.chars().collect()));
            let got = v.run("nlex src").unwrap_or_else(|e| panic!("{src:?}: '{}", e.0));
            let want = v.run("lex src").unwrap();
            assert!(got == want, "{src:?}\n  neant: {}\n  rust:  {}", got.fmt(), want.fmt());
        }
        assert_eq!(v.run("nlex \"1+\\\"x\"").map(|v| v.fmt()).unwrap_or_else(|e| format!("'{}", e.0)), "'lex: unterminated string at line 1");
    }
}

#[cfg(test)]
mod boot_parse {
    use super::*;
    /// The self-hosted parser (boot/parse.nt, fed by boot/lex.nt) must produce the Rust parser's AST.
    #[test]
    fn self_hosted_parser_matches_oracle() {
        let mut v = prelude_vm();
        for f in ["boot/lex.nt", "boot/parse.nt"] { v.run(&std::fs::read_to_string(f).unwrap()).unwrap(); }
        let corpus = [
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
        for src in corpus {
            v.set("src", value::chars(src.chars().collect()));
            let got = v.run("nparse src").unwrap_or_else(|e| panic!("{src:?}: '{}", e.0));
            let want = v.run("parse src").unwrap();
            assert!(got == want, "{src:?}\n  neant: {}\n  rust:  {}", got.fmt(), want.fmt());
        }
        for (src, msg) in [("1+", "parse: incomplete expression at line 1"), ("(1", "parse: missing ) at line 1"), ("x:", "parse: empty assignment at line 1"), ("f[1", "parse: missing ] at line 1"), ("1\n2\n(", "parse: missing ) at line 3"), ("select a", "parse: select needs from at line 1")] {
            v.set("src", value::chars(src.chars().collect()));
            assert_eq!(v.run("nparse src").map(|v| v.fmt()).unwrap_or_else(|e| format!("'{}", e.0)), format!("'{msg}"), "{src:?}");
        }
    }
}

#[cfg(test)]
mod boot_compile {
    use super::*;
    fn boot_vm() -> vm::Vm {
        let mut v = vm::Vm::new();
        for f in BOOT_FILES { v.run(&std::fs::read_to_string(f).unwrap()).unwrap(); }
        v
    }
    /// boot/compile.nt must emit exactly the Rust compiler's bytecode.
    #[test]
    fn self_hosted_compiler_matches_oracle() {
        let mut v = boot_vm();
        let corpus = [
            "x: 1 2 3; 2*x+1", "f:{[a;b] a-b}; f[10;3]", "+/1 2 3 4;+\\x;{x*x}'1 2 3;(+/) 1 2;f/", "$[1<2;`yes;`no];(1;`a;\"s\");$[0;1]",
            "x[0]:9;n+:1;x,:5;g::7;{5}[];f[;3] 10", ":5;{if[x<0; :`neg]; `pos}[-1]", "if[i<n; v[i]: i*i; i+:1]\n\twhile[0;]",
            "{x+y*z};{y};{};{[] 1};{{x} each y};{a:1;b:a+x;a[0]:b;g::a;{c:1;a};b}", "x mod 3;(f each x) over y;{x[i]:1}",
            "fact:{$[x<2;1;x*fact x-1]}", "", ";;", "()",
            "f:{n:10;{x+n}}; add:{[a] {[b] a+b}}; h:{a:1;b:{c:2;{a+c+x}};b[][10]}; {n:1;g:{n};n:2;g[]}",
            "x:(1 2;3 4);x[1;0]:9; c[1]+:10; n:0;do[5;n+:2]; 1 2 3 +\\: 10 20; select sum v by k from t where v>1", "n:0;while[1;n+:1;if[n>4;break]];do[3;break]",
        ];
        for src in corpus {
            v.set("src", value::chars(src.chars().collect()));
            let got = v.run("ncompile src").unwrap_or_else(|e| panic!("{src:?}: '{}", e.0));
            let want = v.run("compile src").unwrap();
            assert!(got == want, "{src:?}\n  neant: {}\n  rust:  {}", got.fmt(), want.fmt());
        }
    }
    /// Every language case, run through the self-hosted pipeline (nlex -> nparse -> ncompile -> exec).
    #[test]
    fn self_hosted_pipeline_runs_language_cases() {
        let mut v = boot_vm();
        let base = v.snapshot();
        for (src, want) in super::tests::CASES {
            v.restore(&base);
            v.set("src", value::chars(src.chars().collect()));
            let got = v.run("nrun src").map(|v| v.fmt()).unwrap_or_else(|e| format!("'{}", e.0));
            assert_eq!(got, *want, "source: {src}");
        }
    }
}

#[cfg(test)]
mod boot_fixpoint {
    use super::*;
    /// Generation 2: rebuild the boot compiler with itself (nrun over its own sources), then it must still
    /// match the Rust oracle and still run the language cases. Also times the whole self-hosted rebuild.
    #[test]
    fn self_hosted_compiler_rebuilds_itself() {
        let mut v = vm::Vm::new();
        let files = BOOT_FILES;
        for f in files { v.run(&std::fs::read_to_string(f).unwrap()).unwrap(); }
        let t = std::time::Instant::now();
        for f in files {
            v.set("src", value::chars(std::fs::read_to_string(f).unwrap().chars().collect()));
            v.run("nrun src").unwrap_or_else(|e| panic!("{f}: '{}", e.0));   // redefines every boot function via the neant pipeline
        }
        let rebuild = t.elapsed();
        for src in ["x: 1 2 3; 2*x+1", "f:{[a;b] a-b}; f[10;3]", "{a:1;b:a+x;a[0]:b;g::a;{c:1;a};b}", "fact:{$[x<2;1;x*fact x-1]}"] {
            v.set("src", value::chars(src.chars().collect()));
            assert!(v.run("ncompile src").unwrap() == v.run("compile src").unwrap(), "gen2 mismatch on {src:?}");
        }
        let base = v.snapshot();
        for (src, want) in super::tests::CASES {
            v.restore(&base);
            v.set("src", value::chars(src.chars().collect()));
            let got = v.run("nrun src").map(|v| v.fmt()).unwrap_or_else(|e| format!("'{}", e.0));
            assert_eq!(got, *want, "gen2 source: {src}");
        }
        assert!(rebuild.as_millis() < 3000, "self-rebuild took {rebuild:?}");
        eprintln!("self-hosted rebuild of boot/*.nt: {rebuild:?}");
    }
}

#[cfg(test)]
mod boot_image {
    use super::*;
    #[test]
    fn image_roundtrip() {
        let mut v = vm::Vm::new();
        let x = v.run("(1;2.5;`a;`b`c;\"s\";\"str\";1 2 3;1.5 2.5;1=1 0;(();`k`j!1 2);$[0;0])").unwrap();
        assert!(image::load(&image::dump(&x).unwrap()).unwrap() == x, "{}", x.fmt());
        assert_eq!(image::load(b"").unwrap_err().0, "image: empty");
    }
    /// boot/boot.nb must be what the current boot sources compile to. If this fails: `neant --build-boot`, rebuild.
    #[test]
    fn embedded_boot_image_is_current() {
        let read = |p: &str| std::fs::read_to_string(p).unwrap();
        let mut vm = stage0(&read);
        let fresh = build_boot_image(&mut vm, &read).unwrap();
        assert!(fresh == BOOT_IMAGE, "boot/boot.nb is stale ({} vs {} bytes): run `cargo run --release -- --build-boot` and rebuild", BOOT_IMAGE.len(), fresh.len());
    }
    /// The embedded image alone (no Rust front end) runs every language case.
    #[test]
    fn embedded_boot_image_runs_language_cases() {
        let mut vm = boot_vm();
        let nrun = vm.get("nrun").unwrap();
        let base = vm.snapshot();
        for (src, want) in super::tests::CASES {
            vm.restore(&base);
            let got = vm.call(&nrun, vec![value::chars(src.chars().collect())]).map(|v| v.fmt()).unwrap_or_else(|e| format!("'{}", e.0));
            assert_eq!(got, *want, "image source: {src}");
        }
    }
}
