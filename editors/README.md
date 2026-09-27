# Editors

The grey text the README shows next to a signature is `neant hints`: the compiler's cost report,
one JSON document per file, for an editor to print. `neant lsp` serves the same document over the
Language Server Protocol, so any editor with an LSP client shows it: `vscode/` talks to it, and
the Neovim and Helix configurations below do.

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

## `neant lsp`

```
neant lsp [-M bytes] [-B bytes]      # JSON-RPC on stdin/stdout, Content-Length framed
```

A Language Server on stdio (bootstrap/src/lsp.rs). It keeps every open buffer, full-text sync,
and analyses one when it opens, changes or is saved — `neant hints` on the buffer, as `--stdin`
does, so `use` resolves from the file's directory and a file need not exist on disk yet. When a
file is saved, the open buffers that `use` it are analysed again. What it answers, all of it read
off that hints document or the module loader:

- `textDocument/publishDiagnostics`: every lex, parse and type error and every broken `#[cost]`
  bound, published to the file it is in — an error in a `use`d file goes to that file — and
  cleared when it is fixed or the buffer is closed. With `initializationOptions:
  {"unknownsAsDiagnostics": true}`, each unknown cost's cause is also a hint-level diagnostic on
  the line the report names.
- `textDocument/inlayHint`: each function's `hint`, the grey text, at the end of its `fn` line.
- `textDocument/hover` on a function's name, where it is defined or called: its grey text and
  every line of the report under it.
- `textDocument/definition` on a function or a struct name: where it is defined, in whichever
  file of the program.

Also `initialize`, `initialized`, `shutdown`, `exit` and `didClose`; anything else is answered
`MethodNotFound`. `tests/lsp/` pins a scripted session (`lsp_session` in bootstrap/tests/lsp.rs).
Analysis is synchronous and whole-program, so a request waits for the last change's analysis; a
`use`d file is read from disk even when its buffer is open and unsaved.

## Neovim

Neovim 0.11 or later, in `init.lua`:

```lua
vim.filetype.add({ extension = { nt = 'neant' } })
vim.lsp.config('neant', {
  cmd = { '/path/to/neant/bootstrap/target/release/neant', 'lsp' },
  filetypes = { 'neant' },
  root_markers = { '.git' },
  init_options = { unknownsAsDiagnostics = true },
})
vim.lsp.enable('neant')
vim.lsp.inlay_hint.enable(true)    -- the grey text
```

Hover is `K`, definition `gd` or `<C-]>` (`vim.lsp.buf.definition`), diagnostics as usual. On an
older Neovim, `vim.lsp.start({ name = 'neant', cmd = { …, 'lsp' } })` from a `FileType neant`
autocommand does the same.

## Helix

In `~/.config/helix/languages.toml`:

```toml
[language-server.neant]
command = "/path/to/neant/bootstrap/target/release/neant"
args = ["lsp"]
config = { unknownsAsDiagnostics = true }

[[language]]
name = "neant"
scope = "source.neant"
file-types = ["nt"]
roots = []
comment-token = "//"
indent = { tab-width = 4, unit = "    " }
language-servers = ["neant"]
```

Inlay hints need `[editor.lsp] display-inlay-hints = true` in `config.toml`. Hover is `space k`,
definition `gd`. Helix has no grammar for neant, so there is no highlighting.

## VS Code

`vscode/` is plain JavaScript against the `vscode` API: no build step, no dependencies.

- Highlighting: `syntaxes/neant.tmLanguage.json`, from the lexer's keywords, literals and
  operators (bootstrap/src/lex.rs) and the four builtin types.
- By default (`neant.mode: lsp`) it starts `neant lsp` and is its client — a small one in
  `extension.js`, with no dependency. It sends each open buffer, and after an edit, once typing
  has paused for `neant.hints.debounceMs` (500 ms), the buffer again; it shows each function's
  inlay hint in grey after its `fn` line, the report in the hover on a function's name, errors in
  the Problems panel (each unknown's cause as a hint-level diagnostic on the line the report
  names), and go-to-definition across `use`d files.
- With `neant.mode: hints`, the fallback, it runs `neant hints` on open and on save, and `neant
  hints --stdin` on the unsaved buffer after a pause, showing the same grey text, hover and
  diagnostics without definitions. Of a multi-file program only what is in the open file is
  shown there. With `debounceMs` 0 it runs on save only, and an edit that adds or removes lines
  clears the hints until then. Switching modes takes a window reload.

Install, from the repository root:

```sh
cargo build --release --manifest-path bootstrap/Cargo.toml
ln -s "$PWD/editors/vscode" ~/.vscode/extensions/neant    # or: code --extensionDevelopmentPath=editors/vscode
```

then set `neant.path` to `bootstrap/target/release/neant` (a relative path is taken from the
workspace folder) and reload the window. Other settings: `neant.mode` (`lsp` or `hints`), `neant.args` (e.g. `["-M", "32768"]`
for another cache), `neant.hints.style` (`decoration`, `inlay` or `off`), `neant.hints.maxLength`,
`neant.hints.debounceMs`, `neant.unknownsAsDiagnostics`, `neant.timeoutMs`. A release build matters on large files: the
concatenated self-hosted compiler takes 65 s under the debug build.
