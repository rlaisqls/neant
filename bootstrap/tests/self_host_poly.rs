//! The polynomial layer's exit test (docs/self-hosting-cost-design.md §5). `compiler/poly.nt` is
//! the data structure the cost calculus is made of, and its correctness argument is entirely
//! **canonical form**: two polynomials are equal exactly when their arenas agree term for term,
//! which only holds if every constructor sorts, merges and reduces.
//!
//! So this pins the arena, not a printed string: per polynomial, the term count and then each
//! term's numerator, denominator and degree, in arena order. A constructor that forgot to merge
//! two equal monomials, or to drop a zero, or to reduce a rational, changes those numbers even
//! when the polynomial it denotes is right.
//!
//! The values were computed by hand. Faulhaber is the one worth checking against a book:
//! `Σ_{k<n} k` is `n²/2 − n/2` and `Σ_{k<n} k²` is `n³/3 − n²/2 + n/6`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn neant() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_neant"))
}

fn repo(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(sub)
}

/// `(what, the neant expression that builds it, the arena it must produce)`. The arena is written
/// as `n_term` then `(cn, cd, degree)` per term, in arena order — which is ascending by monomial,
/// so the lowest-degree term comes first.
const PROBES: &[(&str, &str, &[i64])] = &[
    ("6·n", "pol_add(&facs, &mut terms, &mut pols, &mut pst, t3n, t3n)", &[1, 6, 1, 1]),
    ("n·n − n·n = 0", "pol_sub(&facs, &mut terms, &mut pols, &mut pst, nn, nn)", &[0]),
    ("Σ_{k<n} k = n²/2 − n/2",
     "pol_faulhaber(&mut facs, &mut terms, &mut pols, &mut pst, 1, n, &mut scratch)",
     &[2, -1, 2, 1, 1, 2, 2]),
    ("Σ_{k<n} k² = n³/3 − n²/2 + n/6",
     "pol_faulhaber(&mut facs, &mut terms, &mut pols, &mut pst, 2, n, &mut scratch)",
     &[3, 1, 6, 1, -1, 2, 2, 1, 3, 3]),
    ("Σ_{i<n} 3 = 3·n",
     "pol_sum_over(&mut facs, &mut terms, &mut pols, &mut pst, three, 7, zero, 1, n, &mut scratch)",
     &[1, 3, 1, 1]),
    ("Σ_{i<n} 2·i = n² − n",
     "pol_sum_over(&mut facs, &mut terms, &mut pols, &mut pst, two_i, 7, zero, 1, n, &mut scratch)",
     &[2, -1, 1, 1, 1, 1, 2]),
    // two distinct atoms: the product must sort its factors, and the sum must not merge them
    ("n·m", "pol_mul(&mut facs, &mut terms, &mut pols, &mut pst, n, m)", &[1, 1, 1, 2]),
    ("n + m", "pol_add(&facs, &mut terms, &mut pols, &mut pst, n, m)", &[2, 1, 1, 1, 1, 1, 1]),
    // n² with n := m + 1  →  m² + 2·m + 1
    ("(m+1)² = m² + 2·m + 1",
     "pol_subst(&mut facs, &mut terms, &mut pols, &mut pst, nn, 0, m1)",
     &[3, 1, 1, 0, 2, 1, 1, 1, 1, 2]),
    // a coefficient that must reduce: 6/4 · n → 3/2 · n
    ("6/4·n reduces to 3/2·n", "pol_scale(&facs, &mut terms, &mut pols, &mut pst, n, 6, 4)", &[1, 3, 2, 1]),
    // and one that must vanish rather than linger as a zero term
    ("0·n is the empty polynomial", "pol_scale(&facs, &mut terms, &mut pols, &mut pst, n, 0, 1)", &[0]),
];

#[test]
fn the_polynomial_layer_is_canonical() {
    let poly = std::fs::read_to_string(repo("compiler/poly.nt")).unwrap();
    let dir = std::env::temp_dir().join(format!("neant-poly-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let body: String = PROBES.iter().enumerate()
        .map(|(i, (_, e, _))| format!("    let p{i} = {e};\n    show(&facs, &terms, &pols, p{i});\n"))
        .collect();
    let driver = format!(r#"
fn show(facs: &[Fac], terms: &[Term], pols: &[Pol], p: i64) {{
    println(pols[p].n_term);
    let mut i = 0;
    while i < pols[p].n_term {{
        let t = pols[p].term + i;
        println(terms[t].cn);
        println(terms[t].cd);
        println(mono_degree(facs, terms[t].fac, terms[t].n_fac));
        i += 1;
    }}
}}

fn main() {{
    let mut facs = [Fac {{ atom: 0, exp: 0 }}; 16384];
    let mut terms = [Term {{ fac: 0, n_fac: 0, cn: 0, cd: 0 }}; 16384];
    let mut pols = [Pol {{ term: 0, n_term: 0 }}; 16384];
    let mut pst = [0, 0, 0, 0, 0];
    let mut scratch = [0; 32];

    let n = pol_atom(&mut facs, &mut terms, &mut pols, &mut pst, 0);
    let m = pol_atom(&mut facs, &mut terms, &mut pols, &mut pst, 1);
    let zero = pol_const(&facs, &mut terms, &mut pols, &mut pst, 0);
    let one = pol_const(&facs, &mut terms, &mut pols, &mut pst, 1);
    let three = pol_const(&facs, &mut terms, &mut pols, &mut pst, 3);
    let t3n = pol_mul(&mut facs, &mut terms, &mut pols, &mut pst, three, n);
    let nn = pol_mul(&mut facs, &mut terms, &mut pols, &mut pst, n, n);
    let m1 = pol_add(&facs, &mut terms, &mut pols, &mut pst, m, one);
    let i_at = pol_atom(&mut facs, &mut terms, &mut pols, &mut pst, 7);
    let two_i = pol_scale(&facs, &mut terms, &mut pols, &mut pst, i_at, 2, 1);
{body}}}
"#);
    let nt = dir.join("poly_probe.nt");
    std::fs::write(&nt, format!("{poly}\n{driver}")).unwrap();
    let out = Command::new(neant()).arg("run").arg(&nt).output().unwrap();
    assert!(out.status.success(), "the polynomial probe driver failed:\n{}",
        String::from_utf8_lossy(&out.stderr));
    let got: Vec<i64> = String::from_utf8_lossy(&out.stdout).lines()
        .map(|l| l.trim().parse().unwrap()).collect();

    let mut at = 0;
    let mut failures = Vec::new();
    for (what, _, want) in PROBES {
        let n_term = *got.get(at).unwrap_or(&-999) as usize;
        let len = 1 + n_term * 3;
        let slice: &[i64] = got.get(at..at + len).unwrap_or(&[]);
        if slice != *want {
            failures.push(format!("{what}\n  want {want:?}\n  got  {slice:?}"));
        }
        at += len;
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(at == got.len(), "the driver printed {} values, the probes account for {at}", got.len());
    assert!(failures.is_empty(), "{} of {} polynomials are not what they should be:\n\n{}",
        failures.len(), PROBES.len(), failures.join("\n\n"));
}
