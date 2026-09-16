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

    pub const CASES: &[(&str, &str)] = &[
            ("1+2", "3"),
            ("2*3+4", "14"),                        // right-to-left, no precedence
            ("1 2 3+10", "11 12 13"), ("1 2 3+10 20 30", "11 22 33"), ("1.5+1", "2.5"),
            ("1 - 2", "-1"), ("1 -2", "1 -2"),      // space rule for negative literals
            ("+/1 2 3 4", "10"), ("+\\1 2 3", "1 3 6"), ("*/1 2 3 4", "24"), ("|/3 9 2", "9"), ("+/1.5 2.5", "4f"),
            ("#1 2 3", "3"), ("!5", "0 1 2 3 4"), ("3#1 2", "1 2 1"), ("-2#1 2 3", "2 3"), ("1_1 2 3", "2 3"), ("-1_1 2 3", "1 2"),
            ("0#\"\"", "()"), ("0#()", "()"), ("1#\"\"", "'take from empty"),   // 0 from empty is just empty; only a nonzero take needs elements to cycle through
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
            // group, each over dicts, tables (src/neant/stdlib/table.nt)
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
            ("x:1\n\ny+1", "'undefined: y at line 3"), // runtime errors point at the failing line and unwind a named call stack
            ("f:{x+`a}\nf 1", "'type: arithmetic on non-numeric at line 1\n  in f at line 1\n  at line 2"),
            ("g:{x+`a}\nh:{g x}\nh 1", "'type: arithmetic on non-numeric at line 1\n  in g at line 1\n  in h at line 2\n  at line 3"),
            ("f:{[a;b] a+b}\nf[1;`x]", "'type: arithmetic on non-numeric at line 1\n  in f at line 1\n  at line 2"),
            ("f:{x}\nf[1;2]", "'rank: expected 1 args, got 2 at line 2"),
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
            // asof join, JSON (src/neant/stdlib/json.nt)
            ("t:tbl[`s`t`v;(`a`b`a;1 5 9;10 20 30)];q:tbl[`s`t`p;(`a`a`b;0 8 4;1 2 3)];aj[`s`t;t;q]`p", "1 3 2"), ("t:tbl[`t`v;(1 5 9;10 20 30)];q:tbl[`t`p;(0 8;1 2)];aj[`t;t;q]`p", "1 1 2"),
            ("jk \"{\\\"a\\\": [1, 2.5, \\\"s\\\"], \\\"b\\\": true, \\\"c\\\": null}\"", "`a`b`c!((1f;2.5;\"s\");1b;::)"), ("jk \"[1,2,3]\"", "1 2 3"), ("jk \"[]\"", "()"), ("jk \"{}\"", "()!()"), ("jk \" -1.5e2 \"", "-150f"),
            ("count jk \"\\\"a\\\\nb\\\"\"", "3"), ("jk \"\\\"\\\\u00e9\\\"\"", "\"é\""), ("jk \"{\\\"a\\\":{\\\"b\\\":[{\\\"c\\\":1}]}}\"", ",`a!(,`b!((,`c!,1)))"),
            ("jj `a`b`c!(1 2;\"x\";null)", "\"{\"a\": [1, 2], \"b\": \"x\", \"c\": null}\""), ("jj (1b;0b;null;1.5;`s;4.0)", "\"[true, false, null, 1.5, \"s\", 4]\""), ("jj \"q\\\"\\\\\"", "\"\"q\\\"\\\\\"\""),
            ("jk jj `a`b`c!(1 2;\"x\";null)", "`a`b`c!(1 2;\"x\";::)"), ("jj tbl[`a`b;(1 2;`x`y)]", "\"{\"a\": [1, 2], \"b\": [\"x\", \"y\"]}\""),
            ("jk \"[1,\"", "'json: unexpected end at 3"), ("jk \"[1 2]\"", "'json: expected , or ] at 4"), ("jk \"tru\"", "'json: bad literal at 0"), ("jk \"{\\\"a\\\":1,}\"", "'json: expected key at 7"), ("jj {x}", "'json: cannot serialize a function"),
            // bytes, bit verbs, SHA-256 (src/neant/crypto/crypto.nt)
            ("0x0aff", "0x0aff"), ("type 0x0a", "`byte"), ("0x0aff[1]", "0xff"), ("x: 0x; x,: 0x01; x,: 0x0203; x", "0x010203"), ("`int$0x0aff", "10 255"), ("`byte$255 256 65", "0xff0041"),
            ("`byte$\"hé\"", "0x68c3a9"), ("`char$0x68c3a9", "\"hé\""), ("hex 0x0aff", "\"0aff\""), ("unhex \"0aff\"", "0x0aff"), ("0x0a+1", "11"),
            ("0x0aff bxor 0xff00", "0xf5ff"), ("0x0f band 0x3c", "0x0c"), ("bnot 0x0f", "0xf0"), ("12 bxor 10", "6"), ("255 shl 8", "65280"), ("-1 shr 60", "15"), ("1 2 3 shl 1 2 3", "2 8 24"), ("rotr32[1;1]", "2147483648"),
            ("hex sha256 0x", "\"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\""),
            ("hex sha256 `byte$\"abc\"", "\"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\""),
            ("hex sha256 `byte$\"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq\"", "\"248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1\""),
            // ChaCha20, HMAC/HKDF, Poly1305, AEAD, X25519 (src/neant/crypto/crypto.nt): RFC 8439 / 4231 / 5869 / 7748 vectors
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
            ("}", "'parse: unexpected } at line 1"), (")", "'parse: unexpected ) at line 1"), ("1}", "'parse: unexpected } at line 1"),
            ("(1;2}", "'parse: unexpected } at line 1"), ("f[1]]", "'parse: unexpected ] at line 1"), ("1+2\nx: 3]", "'parse: unexpected ] at line 2"),
            ("// comment\n5", "5"), ("1+2\n3+4", "7"),
            ("f:+;f[1;2]", "3"), ("f:+/;f 1 2 3", "6"), ("(+/)1 2 3", "6"),  // verbs and adverbed verbs are values
            ("+/{x*x} til 1000", "332833500"),
            ("raze (1 2;3 4)", "1 2 3 4"), ("sort 3 1 2", "1 2 3"), ("string 42", "\"42\""), ("`$\"ab\"", "`ab"), ("type 1 2", "`ints"),
            // index assignment, compound assignment
            ("x:1 2 3;x[1]:9;x", "1 9 3"), ("x:1 2 3;x[0 2]:7;x", "7 2 7"), ("x:1 2 3;x[1]:`a;x", "(1;`a;3)"),
            ("x:1 2 3;x[5]:1", "'index: amend out of range"), ("x:1 2 3;@[{x[5]:1};0;0];x", "1 2 3"),
            ("d:`a`b!1 2;d[`c]:3;d", "`a`b`c!1 2 3"), ("d:`a`b!1 2;d[`a]:9;d`a", "9"),
            ("d:`a`b!1 2;d[`c]:10 20;d`c", "10 20"), ("d:`a`b!1 2;d[`c]:10 20;count d", "3"),   // a vector value is one entry
            ("d:()!();d[7]:1 2 3;d 7", "1 2 3"), ("d:`a!,1 2;d[`b]:3 4;d`a", "1 2"),
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
            ("@[{1+`a};0;{elast `line}]", "1"), ("@[{1+`a};0;{elast `trace}]", "((`;1))"), ("elast `nope", "'type: elast `line or elast `trace"),
            // strings, casts
            ("\",\" vs \"a,b,c\"", "(\"a\";\"b\";\"c\")"), ("\",\" sv (\"ab\";\"cd\")", "\"ab,cd\""), ("\"hello\" ss \"l\"", "2 3"),
            ("upper \"ab\"", "\"AB\""), ("trim \"  a \"", "\"a\""), ("\"\\n\" vs \"a\\nb\"", "(\"a\";\"b\")"),
            ("`int$\"42\"", "42"), ("`float$\"1.5\"", "1.5"), ("`int$3.7", "3"), ("`char$65 66", "\"AB\""), ("`code$\"A\"", "65"),
            ("`int$(\"1\";\"22\")", "1 22"), ("`int$\"x\"", "'parse: not an int: \"x\""),
            ("\"abc\"[1]", "\"b\""), ("\"abc\"~\"abc\"", "1b"), ("\"abc\"=\"abd\"", "110b"),
            // a name the parser reads as a verb cannot be read back as a local, so it is rejected outright
            ("{[sv] sv}", "'name: `sv is an infix verb, so it cannot be a local — rename it at line 1"),
            ("{cut: 1; bin: 2; 0}", "'name: `cut, `bin are infix verbs, so they cannot be locals — rename them at line 1"),
            ("\",\" sv (\"a\";\"b\")", "\"a,b\""),   // the global of that name is still the verb
            ("nrun \"1+1\"\n2", "2"),   // a nested nrun (what `load` is) must not clobber the caller's line table
            // `f ,x` reads as `f , x`, which used to build a two-element list and fail far from the cause
            ("f:{x};f,1", "'type: , has a function on its left — `f ,x` parses as `f , x`, so write `f (,x)`"),
            ("f:{x};f (,1)", ",1"), ("(1 2),3", "1 2 3"), ("x:();x,:{y};count x", "1"),
            // urand is the OS pool, for keys; rand stays the reproducible PRNG it is documented as
            ("count urand 32", "32"), ("type urand 8", "`bytes"), ("(urand 8)~urand 8", "0b"),
            ("rseed 7; x: 8 rand 256; rseed 7; x ~ 8 rand 256", "1b"),
            ("undefined_name", "'undefined: undefined_name"), ("1 2+1 2 3", "'length"), ("{x}[1;2]", "'rank: expected 1 args, got 2"),
        ];

    /// Every language case through the whole self-hosted pipeline: nlex -> nparse -> ncompile -> exec.
    #[test]
    fn language() {
        let mut v = boot_vm();
        let base = v.snapshot();
        for (src, want) in CASES {
            v.restore(&base);
            assert_eq!(ev(&mut v, src), *want, "source: {src}");
        }
    }

    /// Every tests/*.nt through tests/run.nt, the same path the file runner takes (cwd is the crate root under
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
    /// single call is enough for it to take the loop over partway through. `fDo` is the same body
    /// as a `do` loop, which no trace is ever started on (its counter is live on the operand stack
    /// at the backward jump, and `run_ops` only starts recording on an empty one, src/vm.rs), so
    /// it stays interpreted however many iterations it runs. It does marginally *less* work per
    /// iteration than `f` — no `i<k` compare — so if anything this understates the ratio, which
    /// is the right direction for a baseline to be wrong in.
    #[test]
    #[ignore]
    fn manual_perf_measurement() {
        let mut v = boot_vm();
        v.eval("f: {[k] n:0; i:0; while[i<k; n: n+i*i; i: i+1]; n}").unwrap();
        v.eval("fDo: {[k] n:0; i:0; do[k; n: n+i*i; i: i+1]; n}").unwrap();
        let t0 = std::time::Instant::now();
        v.eval("fDo 1000000").unwrap();
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
    /// trace can only hit it *mid-loop*, with earlier iterations' writes already committed. So
    /// this is the one deopt that has to undo something: it restores the snapshot the compiled
    /// code re-takes at the top of every iteration and resumes at the loop header
    /// (`jitCompileTrace`, src/neant/jit/arm64.nt). Starting at `0W-200` is what puts the
    /// collision ~200 iterations in — well after the trace was recorded and compiled, which
    /// starting at `0W` would not (`n` would already be null by then, and a null local is refused
    /// at the recorder).
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
    /// same `do`-loop baseline `manual_perf_measurement` above uses, and for the same reason.
    #[test]
    #[ignore]
    fn manual_trace_perf_measurement() {
        let mut v = boot_vm();
        v.eval("f: {[k] n:0; i:0; while[i<k; n: n+i*i; i: i+1]; n}").unwrap();
        v.eval("fDo: {[k] n:0; i:0; do[k; n: n+i*i; i: i+1]; n}").unwrap();
        let t0 = std::time::Instant::now();
        let slow = v.eval("fDo 5000000").unwrap();
        let interpreted = t0.elapsed();
        let t1 = std::time::Instant::now();
        let fast = v.eval("f 5000000").unwrap();
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
    /// compile the corpora byte-identically and still run every language case. This is what the Rust oracle
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
        let base = v.snapshot();
        for (src, want) in super::tests::CASES {
            v.restore(&base);
            assert_eq!(super::tests::ev(&mut v, src), *want, "gen2 source: {src}");
        }
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

