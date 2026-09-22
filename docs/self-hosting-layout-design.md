# Self-hosting: the layout choice

Nine of the thirteen differences between `neant cost` and the self-hosted pass have one cause, and
it is not an analysis gap. The Rust compiler **chooses** between array-of-structs and
struct-of-arrays per struct type, from the cost model; the self-hosted emitter always emits AoS,
and so reports AoS traffic.

`docs/self-hosting-arrays-design.md` §4 chose that deliberately, and gave the reason:

> The layout decision belongs to the cost calculus, which is not self-hosted, and choosing it
> without one would be a guess dressed as a decision.

**That premise has expired.** The cost calculus is now self-hosted — 72 `work` columns with no
declines, 56 `moves` columns — so the self-hosted compiler can make the decision on the same
evidence the Rust one uses, rather than inheriting a default.

This is the first piece of work here that changes what the compiler **emits**, not what it
computes. Everything before it was analysis.

## 1. What the choice is

For each struct not pinned by `#[layout(...)]`: analyse the whole module twice, once with that
struct AoS and once SoA, evaluate every touching function's `moves` at a reference point, and sum.
**Strictly fewer bytes wins; a tie is AoS.** `choose_layouts` in `analyze.rs`, and the reference
point is `B` and `M` from the machine with every size variable at `10⁶`.

On this corpus it picks SoA three times and AoS twice, and the reasons are legible:

```
Point     SoA   sum_x   8·ps.len() + B   against AoS  24·ps.len() + B
Particle  SoA   kinetic 16·ps.len() + 2·B against AoS  32·ps.len() + B
Cell      SoA   tagged  9·cs.len() + 2·B  against AoS  32·cs.len() + 2·B
Node      AoS   sum_list — a pointer chase reads the whole node either way
TNode     AoS   main — SoA costs 13·B where AoS costs 7·B
```

A function that reads one field of three pays for one field under SoA and for the whole element
under AoS; a function that reads every field pays the same under both, plus one stream per field.
That is the entire argument, and it is why `sum_x` decides `Point` and `sum_all` does not.

## 2. What the corpus actually needs, which is much less than SoA in general

Every operation on an array of structs in every in-slice golden:

```neant
fn sum_x(ps: &[Point]) -> f64 { … ps[i].x … }      // a field read
fn shift(ps: &mut [Point], d: f64) { ps[i].x += d; } // a field write
let mut ps = [Point { x: 1.0, y: 2.0, z: 3.0 }; 4];  // a repeat build
ps[2].y = 9.0;                                        // a field write
```

**No golden ever reads or writes a whole element** — no `ps[i]`, no `ps[i] = Point { … }`, no
struct-array argument passed by value, no struct-array return. Checked across the corpus, not
assumed.

So the emitter needs exactly four forms, and each is a small change:

| form | AoS | SoA |
|---|---|---|
| parameter `ps: &[Point]` | `const struct nt_Point *ps_p, int64_t ps_n` | one pointer per field, then `int64_t ps_n` |
| `ps[i].x` | `ps_p[nt_idx(i, ps_n, ℓ)].x` | `ps_x_p[nt_idx(i, ps_n, ℓ)]` |
| `let ps = [Point { … }; n]` | one allocation, one fill loop | one allocation and one fill per field |
| `&ps` as an argument | `ps_p, ps_n` | every field pointer, then `ps_n` |

A whole-element access is **out of the slice** and must say so rather than emit something plausible
— the emitter's refusal flag (`est[2]`) already exists for this, and a struct array that is read
whole makes the program unsupported rather than wrong.

## 3. What the cost pass needs

- **A layout per struct**, consulted by `elem_bytes` and by the site's stride. Under SoA a field
  access is a site on *that field's* array, with the field's own size as the stride; under AoS it
  is a site on the element, with the element's size as the stride and the field's as what is
  touched. That distinction — `es` versus `stride` — is the one M4 recorded as making AoS cost
  `24n` and SoA `8n`.
- **A numeric evaluation** of a `moves` polynomial at the reference point: `B = 64`, `M = 2²¹`,
  every size atom `10⁶`, in `f64`. The polynomial layer has no `eval` yet; it is twenty lines.
- **The choice loop** itself: for each struct, for each layout, walk every function; sum the
  touching ones; keep the winner. The walks are pure, so this is repetition rather than new
  machinery.

A function whose `moves` this slice declines contributes nothing to either total, exactly as an
`Unknown` contributes `0.0` in `choose_layouts`. That is worth stating because it means the
self-hosted choice is made on **less evidence** than the Rust one wherever the slice is narrower —
and `matmul`, `stencil` and `tri` are declined. None of them has a struct, so on this corpus the
evidence is the same; on another corpus it would not be, and the report should not pretend
otherwise.

## 4. Exit test

Two, and the first is the one that matters:

1. **`self_hosted_emit_runs_the_same` must stay green.** Five golden programs with struct arrays go
   through the whole self-hosted chain and are run; if SoA is emitted wrongly they print different
   numbers. This is a behavioural test over emitted code, which is the only kind that cannot be
   satisfied by two implementations agreeing on a representation.
2. **`AOS_INSTEAD` empties.** Nine `moves` differences close, and the list that records them should
   be deleted rather than shortened — if it still has entries, the choice did not reproduce.

The risk is worth naming: this is the first change that can make the self-hosted compiler emit
**wrong code** rather than merely report a wrong number. The behavioural test is the reason it is
safe to attempt; without it this would not be worth the exposure.
