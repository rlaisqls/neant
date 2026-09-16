# Editor support

Three pieces, none of them required to use the language:

| | |
|---|---|
| `tree-sitter-neant/` | a tree-sitter grammar — highlighting, folding, textobjects |
| `vim/` | a vim syntax file and an ftdetect rule for `.nt` |
| `../src/neant/tools/lsp.nt` | a language server, written in neant |

The grammar and the syntax file are both approximations of the lexer by design — their own header
comments say where they diverge and why. The language server is not an approximation: it runs the
real front end (`nlex`, `nparse`) out of the boot image, so a diagnostic it reports is the error
the compiler would give, on the line the compiler would name.

## What the server does

`initialize` advertises exactly what is implemented, and nothing else:

- **diagnostics** — the buffer goes through `nlex` then `nparse` inside `@[..]` on every open and
  every change; a signalled error becomes one diagnostic on the line its message names.
- **formatting** — `nfmt` (`../src/neant/tools/fmt.nt`), returned as one whole-buffer edit.
- **document symbols** — the top-level `name:` assignments, `{`-valued ones as functions.
- **hover** — the definition line and the comment block written above it for a name defined in the
  buffer; for anything else, the value's type out of the running image (a lambda's parameter list,
  a verb binding like `sum : +/`).
- **completion** — the same two sets, filtered by what has been typed.

Full document sync (capability `1`): every change carries the whole buffer.

## Pointing an editor at it

The server speaks both transports. **Stdio** is the default an editor launches, and is what you
want unless you have a reason otherwise; `read1`/`write1` (src/prims.rs) make the framing
byte-exact, so each request is answered as it arrives:

```
./target/release/neant src/neant/tools/lsp.nt
```

**TCP** is the same loop on a socket, for attaching to an already-running server:

Start it from the repository root — it looks for `src/neant/...` there to learn the names in the
image, and falls back to just the built-ins if it cannot find them (`rootUri` from `initialize` is
tried as a prefix too, so a client that sends one may start the server anywhere):

```
./target/release/neant src/neant/tools/lsp.nt 127.0.0.1:5007
```

It serves one connection, which is what one editor opens, and exits when that connection closes or
the client sends `exit`.

### Neovim

```lua
vim.filetype.add({ extension = { nt = "neant" } })
vim.api.nvim_create_autocmd("FileType", {
  pattern = "neant",
  callback = function(args)
    vim.lsp.start({
      name = "neant",
      cmd = { "./target/release/neant", "src/neant/tools/lsp.nt" },   -- or vim.lsp.rpc.connect("127.0.0.1", 5007)
      root_dir = vim.fs.root(args.buf, { "Cargo.toml" }),
    }, { bufnr = args.buf })
  end,
})
```

`gq`/`:lua vim.lsp.buf.format()` then runs `nfmt`, `gO` lists the symbols, `K` hovers.

### Emacs (eglot)

```elisp
(add-to-list 'auto-mode-alist '("\\.nt\\'" . prog-mode))
(add-to-list 'eglot-server-programs
             '(prog-mode . ("./target/release/neant" "src/neant/tools/lsp.nt")))   ; or ("127.0.0.1" 5007)
```

### VS Code

There is no published extension. A client extension launches it over stdio like any other server:

```js
const serverOptions = {
  command: "./target/release/neant",
  args: ["src/neant/tools/lsp.nt"],
};
```

### Helix and other stdio-only clients

Nothing special — stdio is the transport they use:

```toml
[language-server.neant]
command = "./target/release/neant"
args = ["src/neant/tools/lsp.nt"]
```

## Formatting without an editor

`nfmtFile` rewrites a file in place and says whether it changed anything:

```
printf 'load "src/neant/tools/fmt.nt"\nnfmtFile "src/neant/stdlib/json.nt"\n' | ./target/release/neant
```
