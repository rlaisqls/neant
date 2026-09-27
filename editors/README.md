# Editors

The grey text the README shows next to a signature is `neant hints`: the compiler's cost report,
one JSON document per file, for an editor to print. `vscode/` is the one editor that reads it so
far.

## `neant hints`

```
neant hints f.nt [-M bytes] [-B bytes]
neant hints --stdin f.nt      # the source on stdin: an unsaved buffer, its `use`s resolved from f.nt's directory
```

With `--stdin`, `f.nt` only names the buffer — it need not exist yet — and every path in the
document is as it would be for the file. `tests/hints/stdin/` pins it (`hints_stdin` in
bootstrap/tests/hints.rs).

Always prints one document on stdout and exits 0, so an editor never reads stderr:

```json
{
  "file": "tests/golden/fib.nt",
  "diagnostics": [],
  "functions": [
    {"name": "fib", "line": 1, "tier": "unknown", "hint": "unknown: 2 recursive calls each shrinking `n` by a constant: exponential", "cause": "…", "cause_line": 3, …},
    {"name": "main", "line": 10, "tier": "modulo", "hint": "work work[fib](20) + 102   moves moves[fib](20)   modulo, io", "rests_on": ["fib (unknown)"], …}
  ]
}
```

Per function: `name`, `file`, `line` (of the `fn`, 1-based), `tier` (`exact`, `recurrence`,
`modulo`, `bound`, `declared`, `unknown`), `hint` (the grey text), `work` and `moves` in full as
the lockfile states them, `span` where it differs from work, `regimes` (each piece of a piecewise
`moves` with its condition), `declared_work`/`declared_moves` from `#[cost]`, `effects`,
`bounds` and `footprint` (the report's lines), `rests_on`, `cause` and `cause_line` for an
unknown, `violations`, and `report` — every line `neant cost` prints under the function. A lex,
parse or type error, or a broken `#[cost]` bound, is an entry of `diagnostics` with `line`, `col`,
`severity` and `message`. Nothing in it is computed for the editor; it is the report taken apart.
`tests/hints/` pins it (bootstrap/tests/hints.rs).

## VS Code

`vscode/` is plain JavaScript against the `vscode` API: no build step, no dependencies.

- Highlighting: `syntaxes/neant.tmLanguage.json`, from the lexer's keywords, literals and
  operators (bootstrap/src/lex.rs) and the four builtin types.
- On open and on save it runs `neant hints` on the file, and after an edit, once typing has
  paused for `neant.hints.debounceMs` (500 ms), `neant hints --stdin` on the unsaved buffer; it
  shows each function's `hint` in grey after its `fn` line, with the full report in the hover;
  errors go to the Problems panel, and each unknown's cause is a hint-level diagnostic on the line
  the report names. Of a multi-file program only what is in the open file is shown there. With
  `debounceMs` 0 it runs on save only, and an edit that adds or removes lines clears the hints
  until then.

Install, from the repository root:

```sh
cargo build --release --manifest-path bootstrap/Cargo.toml
ln -s "$PWD/editors/vscode" ~/.vscode/extensions/neant    # or: code --extensionDevelopmentPath=editors/vscode
```

then set `neant.path` to `bootstrap/target/release/neant` (a relative path is taken from the
workspace folder) and reload the window. Other settings: `neant.args` (e.g. `["-M", "32768"]`
for another cache), `neant.hints.style` (`decoration`, `inlay` or `off`), `neant.hints.maxLength`,
`neant.hints.debounceMs`, `neant.unknownsAsDiagnostics`, `neant.timeoutMs`. A release build matters on large files: the
concatenated self-hosted compiler takes 65 s under the debug build.
