// The editor surface of neant. By default it talks to `neant lsp`, the compiler as a Language
// Server (a small client below: JSON-RPC over the child's stdio), which answers diagnostics,
// inlay hints, hovers and definitions on the live buffer. With `neant.mode` set to `hints` it
// runs `neant hints` instead: on open, on save, and — debounced — as the buffer changes (on the
// live buffer through `--stdin`, on the file on save). Either way each function's cost is grey
// text after its `fn` line, the report under it in a hover, and every error in the Problems
// panel. Nothing here computes a cost; it prints what the compiler said.
// Plain JavaScript against the vscode API, no build step and no dependencies.

'use strict';

const vscode = require('vscode');
const cp = require('child_process');
const path = require('path');

/** Results by document URI: the parsed `neant hints` document of the last run. */
const results = new Map();
/** Runs in flight by URI, so a save during a run kills the stale one. */
const running = new Map();
/** Pending debounced runs on the live buffer, by URI. */
const timers = new Map();

let diagnostics;
let decoration;
let inlayChanged;
let output;
let warnedMissing = false;

function config() {
  return vscode.workspace.getConfiguration('neant');
}

function neantPath(doc) {
  const p = config().get('path') || 'neant';
  if (!p.includes('/') && !p.includes('\\')) return p;
  if (path.isAbsolute(p)) return p;
  const folder = vscode.workspace.getWorkspaceFolder(doc.uri);
  return path.join(folder ? folder.uri.fsPath : path.dirname(doc.uri.fsPath), p);
}

function isNeant(doc) {
  return doc.languageId === 'neant' && doc.uri.scheme === 'file';
}

/** `live`: the buffer as it is now goes on stdin, and `use` still resolves from the file's path. */
function run(doc, live) {
  if (!isNeant(doc)) return;
  const key = doc.uri.toString();
  const prev = running.get(key);
  if (prev) prev.kill();
  const args = ['hints', ...(config().get('args') || []), ...(live ? ['--stdin'] : []), doc.uri.fsPath];
  const child = cp.execFile(neantPath(doc), args, {
    cwd: path.dirname(doc.uri.fsPath),
    timeout: config().get('timeoutMs') || 120000,
    maxBuffer: 64 * 1024 * 1024,
  }, (err, stdout, stderr) => {
    // a later run replaced this one: its answer is for text that is gone
    if (running.get(key) !== child) return;
    running.delete(key);
    if (err && err.code === 'ENOENT') {
      if (!warnedMissing) {
        warnedMissing = true;
        vscode.window.showWarningMessage(`neant: cannot run \`${neantPath(doc)}\`; set neant.path to the compiler (bootstrap/target/debug/neant).`);
      }
      return;
    }
    let doc_;
    try {
      doc_ = JSON.parse(stdout);
    } catch (e) {
      // not a hints document: the file could not be read, or the flag is unknown to this build
      output.appendLine(`neant hints ${doc.uri.fsPath}: ${(stderr || String(err || e)).trim()}`);
      diagnostics.set(doc.uri, [new vscode.Diagnostic(new vscode.Range(0, 0, 0, 0),
        `neant hints failed: ${(stderr || String(err || e)).trim()}`, vscode.DiagnosticSeverity.Error)]);
      return;
    }
    results.set(key, doc_);
    publish(doc, doc_);
  });
  if (live) {
    child.stdin.on('error', () => {});
    child.stdin.end(doc.getText());
  }
  running.set(key, child);
}

/** Whether a function or diagnostic of a multi-file program is in this document: each names its
 *  own file, as reached from the directory `neant` ran in, which is the document's. */
function here(doc, x) {
  return !x.file || path.resolve(path.dirname(doc.uri.fsPath), x.file) === doc.uri.fsPath;
}

/** A position from the compiler's 1-based line and column, clamped to the document. */
function at(doc, line, col) {
  const l = Math.min(Math.max((line || 1) - 1, 0), Math.max(doc.lineCount - 1, 0));
  const text = doc.lineAt(l).text;
  const c = Math.min(Math.max((col || 1) - 1, 0), text.length);
  return new vscode.Position(l, c);
}

/** The word at a position, or the rest of the line when there is none, as a diagnostic's range. */
function span(doc, pos) {
  const w = doc.getWordRangeAtPosition(pos);
  if (w) return w;
  return new vscode.Range(pos, doc.lineAt(pos.line).range.end);
}

function publish(doc, hints) {
  const list = [];
  for (const d of (hints.diagnostics || []).filter((d) => here(doc, d))) {
    const sev = d.severity === 'warning' ? vscode.DiagnosticSeverity.Warning : vscode.DiagnosticSeverity.Error;
    const diag = new vscode.Diagnostic(span(doc, at(doc, d.line, d.col)), d.message, sev);
    diag.source = 'neant';
    list.push(diag);
  }
  if (config().get('unknownsAsDiagnostics')) {
    for (const f of (hints.functions || []).filter((f) => here(doc, f))) {
      if (f.tier !== 'unknown' || !f.cause_line) continue;
      const pos = at(doc, f.cause_line, 1);
      const line = doc.lineAt(pos.line);
      const range = new vscode.Range(pos.line, line.firstNonWhitespaceCharacterIndex, pos.line, line.text.length);
      const diag = new vscode.Diagnostic(range, `\`${f.name}\`'s cost is unknown: ${f.cause}`, vscode.DiagnosticSeverity.Hint);
      diag.source = 'neant cost';
      list.push(diag);
    }
  }
  diagnostics.set(doc.uri, list);
  paint();
  inlayChanged.fire();
}

function clip(s) {
  const max = config().get('hints.maxLength') || 120;
  return s.length > max ? s.slice(0, max - 1) + '…' : s;
}

/** The hover: the function's line and everything the report prints under it. */
function hover(f) {
  const md = new vscode.MarkdownString();
  const lines = [`${f.name}  ${f.hint}`];
  if (f.work !== null && f.hint !== `work ${f.work}   moves ${f.moves}   ${f.tier}`) {
    lines.push(`work   ${f.work}`, `moves  ${f.moves}`);
  }
  if (f.span) lines.push(`span   ${f.span}`);
  for (const r of f.report || []) lines.push(r);
  md.appendCodeblock(lines.join('\n'), 'text');
  return md;
}

function paint() {
  const style = config().get('hints.style');
  for (const editor of vscode.window.visibleTextEditors) {
    const doc = editor.document;
    if (!isNeant(doc)) continue;
    const hints = results.get(doc.uri.toString());
    if (!hints || style !== 'decoration') { editor.setDecorations(decoration, []); continue; }
    const opts = (hints.functions || []).filter((f) => here(doc, f) && f.line >= 1 && f.line <= doc.lineCount).map((f) => {
      const end = doc.lineAt(f.line - 1).range.end;
      return {
        range: new vscode.Range(end, end),
        hoverMessage: hover(f),
        renderOptions: { after: { contentText: `  // ${clip(f.hint)}` } },
      };
    });
    editor.setDecorations(decoration, opts);
  }
}

const inlayProvider = {
  onDidChangeInlayHints: null,
  provideInlayHints(doc, range) {
    if (config().get('hints.style') !== 'inlay') return [];
    const hints = results.get(doc.uri.toString());
    if (!hints) return [];
    return (hints.functions || []).filter((f) => here(doc, f))
      .filter((f) => f.line >= 1 && f.line <= doc.lineCount && f.line - 1 >= range.start.line && f.line - 1 <= range.end.line)
      .map((f) => {
        const h = new vscode.InlayHint(doc.lineAt(f.line - 1).range.end, `// ${clip(f.hint)}`);
        h.paddingLeft = true;
        h.tooltip = hover(f);
        return h;
      });
  },
};

// ---------------------------------------------------------------- `neant lsp`

/** The server: the child, its unparsed output, and requests waiting for an answer by id. */
let server = null;

function workspaceDoc() {
  const f = (vscode.workspace.workspaceFolders || [])[0];
  return { uri: f ? vscode.Uri.file(path.join(f.uri.fsPath, 'x.nt')) : vscode.Uri.file(path.join(process.cwd(), 'x.nt')) };
}

function send(msg) {
  if (!server) return;
  const body = Buffer.from(JSON.stringify({ jsonrpc: '2.0', ...msg }), 'utf8');
  server.child.stdin.write(`Content-Length: ${body.length}\r\n\r\n`);
  server.child.stdin.write(body);
}

function request(method, params) {
  if (!server) return Promise.resolve(null);
  const id = ++server.next;
  return new Promise((resolve) => {
    server.pending.set(id, resolve);
    send({ id, method, params });
  });
}

function notify(method, params) { send({ method, params }); }

function received(msg) {
  if (msg.id !== undefined && server.pending.has(msg.id)) {
    const resolve = server.pending.get(msg.id);
    server.pending.delete(msg.id);
    if (msg.error) output.appendLine(`neant lsp: ${msg.error.message}`);
    resolve(msg.error ? null : msg.result);
    return;
  }
  if (msg.method === 'textDocument/publishDiagnostics') {
    const uri = vscode.Uri.parse(msg.params.uri);
    diagnostics.set(uri, msg.params.diagnostics.map((d) => {
      const r = d.range;
      const diag = new vscode.Diagnostic(new vscode.Range(r.start.line, r.start.character, r.end.line, r.end.character), d.message, d.severity - 1);
      diag.source = d.source;
      return diag;
    }));
    // the analysis behind the grey text is new: ask for it again
    paintLsp();
    inlayChanged.fire();
  }
}

function startServer() {
  const doc = workspaceDoc();
  const child = cp.spawn(neantPath(doc), ['lsp', ...(config().get('args') || [])], {
    cwd: path.dirname(doc.uri.fsPath),
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  server = { child, next: 0, pending: new Map(), buf: Buffer.alloc(0), opened: new Set() };
  const me = server;
  child.on('error', (e) => {
    if (e.code === 'ENOENT' && !warnedMissing) {
      warnedMissing = true;
      vscode.window.showWarningMessage(`neant: cannot run \`${neantPath(doc)}\`; set neant.path to the compiler (bootstrap/target/debug/neant).`);
    } else output.appendLine(`neant lsp: ${e}`);
  });
  child.on('exit', (code) => {
    if (server === me) { output.appendLine(`neant lsp exited (${code})`); server = null; }
  });
  child.stdin.on('error', () => {});
  child.stderr.on('data', (d) => output.append(d.toString()));
  child.stdout.on('data', (chunk) => {
    me.buf = Buffer.concat([me.buf, chunk]);
    for (;;) {
      const end = me.buf.indexOf('\r\n\r\n');
      if (end < 0) return;
      const m = /Content-Length:\s*(\d+)/i.exec(me.buf.slice(0, end).toString());
      const len = m ? Number(m[1]) : 0;
      if (me.buf.length < end + 4 + len) return;
      const body = me.buf.slice(end + 4, end + 4 + len).toString('utf8');
      me.buf = me.buf.slice(end + 4 + len);
      try { if (server === me) received(JSON.parse(body)); } catch (e) { output.appendLine(`neant lsp: ${e}`); }
    }
  });
  request('initialize', {
    processId: process.pid,
    rootUri: (vscode.workspace.workspaceFolders || [])[0]?.uri.toString() || null,
    capabilities: {},
    initializationOptions: { unknownsAsDiagnostics: !!config().get('unknownsAsDiagnostics') },
  }).then(() => {
    notify('initialized', {});
    vscode.workspace.textDocuments.forEach(openLsp);
  });
}

function stopServer() {
  if (!server) return;
  const s = server;
  server = null;
  for (const t of timers.values()) clearTimeout(t);
  timers.clear();
  const frame = (m) => { const b = Buffer.from(JSON.stringify({ jsonrpc: '2.0', ...m }), 'utf8'); return `Content-Length: ${b.length}\r\n\r\n${b}`; };
  s.child.stdin.write(frame({ id: 0, method: 'shutdown' }));
  s.child.stdin.end(frame({ method: 'exit' }));
  setTimeout(() => s.child.kill(), 2000);
  diagnostics.clear();
}

function openLsp(doc) {
  if (!server || !isNeant(doc) || server.opened.has(doc.uri.toString())) return;
  server.opened.add(doc.uri.toString());
  notify('textDocument/didOpen', { textDocument: { uri: doc.uri.toString(), languageId: 'neant', version: doc.version, text: doc.getText() } });
}

function changeLsp(doc) {
  if (!server || !server.opened.has(doc.uri.toString())) return;
  notify('textDocument/didChange', { textDocument: { uri: doc.uri.toString(), version: doc.version }, contentChanges: [{ text: doc.getText() }] });
}

/** The decoration style: the server's inlay hints, drawn as grey text after the line. */
function paintLsp() {
  if (!server) return;
  const style = config().get('hints.style');
  for (const editor of vscode.window.visibleTextEditors) {
    const doc = editor.document;
    if (!isNeant(doc)) continue;
    if (style !== 'decoration') { editor.setDecorations(decoration, []); continue; }
    request('textDocument/inlayHint', { textDocument: { uri: doc.uri.toString() }, range: { start: { line: 0, character: 0 }, end: { line: doc.lineCount, character: 0 } } }).then((hs) => {
      editor.setDecorations(decoration, (hs || []).map((h) => {
        const p = new vscode.Position(h.position.line, h.position.character);
        return { range: new vscode.Range(p, p), renderOptions: { after: { contentText: `  // ${clip(h.label)}` } } };
      }));
    });
  }
}

function activateLsp(context) {
  context.subscriptions.push(
    vscode.languages.registerInlayHintsProvider({ language: 'neant', scheme: 'file' }, {
      onDidChangeInlayHints: inlayChanged.event,
      async provideInlayHints(doc, range) {
        if (config().get('hints.style') !== 'inlay') return [];
        const hs = await request('textDocument/inlayHint', { textDocument: { uri: doc.uri.toString() }, range: { start: range.start, end: range.end } });
        return (hs || []).map((h) => {
          const x = new vscode.InlayHint(new vscode.Position(h.position.line, h.position.character), `// ${clip(h.label)}`);
          x.paddingLeft = true;
          return x;
        });
      },
    }),
    vscode.languages.registerHoverProvider({ language: 'neant', scheme: 'file' }, {
      async provideHover(doc, pos) {
        const h = await request('textDocument/hover', { textDocument: { uri: doc.uri.toString() }, position: { line: pos.line, character: pos.character } });
        if (!h) return null;
        const r = h.range;
        return new vscode.Hover(new vscode.MarkdownString(h.contents.value), r && new vscode.Range(r.start.line, r.start.character, r.end.line, r.end.character));
      },
    }),
    vscode.languages.registerDefinitionProvider({ language: 'neant', scheme: 'file' }, {
      async provideDefinition(doc, pos) {
        const d = await request('textDocument/definition', { textDocument: { uri: doc.uri.toString() }, position: { line: pos.line, character: pos.character } });
        if (!d) return null;
        const r = d.range;
        return new vscode.Location(vscode.Uri.parse(d.uri), new vscode.Range(r.start.line, r.start.character, r.end.line, r.end.character));
      },
    }),
    vscode.workspace.onDidOpenTextDocument(openLsp),
    vscode.workspace.onDidChangeTextDocument((e) => {
      if (!isNeant(e.document) || e.contentChanges.length === 0) return;
      // the server analyses every change it is sent: send the buffer once typing pauses
      const key = e.document.uri.toString();
      clearTimeout(timers.get(key));
      timers.set(key, setTimeout(() => { timers.delete(key); changeLsp(e.document); }, Math.max(config().get('hints.debounceMs') || 0, 0)));
    }),
    vscode.workspace.onDidSaveTextDocument((d) => {
      if (!server || !server.opened.has(d.uri.toString())) return;
      const key = d.uri.toString();
      if (timers.has(key)) { clearTimeout(timers.get(key)); timers.delete(key); changeLsp(d); }
      notify('textDocument/didSave', { textDocument: { uri: key } });
    }),
    vscode.workspace.onDidCloseTextDocument((d) => {
      if (!server || !server.opened.delete(d.uri.toString())) return;
      notify('textDocument/didClose', { textDocument: { uri: d.uri.toString() } });
    }),
    vscode.window.onDidChangeVisibleTextEditors(paintLsp),
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (!e.affectsConfiguration('neant')) return;
      if (e.affectsConfiguration('neant.mode')) {
        vscode.window.showInformationMessage('neant: reload the window to switch between `neant lsp` and `neant hints`.');
        return;
      }
      warnedMissing = false;
      stopServer();
      startServer();
    }),
    vscode.commands.registerCommand('neant.refreshHints', () => { stopServer(); startServer(); }),
    { dispose: stopServer },
  );
  startServer();
}

// ---------------------------------------------------------------- activation

function activate(context) {
  output = vscode.window.createOutputChannel('neant');
  diagnostics = vscode.languages.createDiagnosticCollection('neant');
  decoration = vscode.window.createTextEditorDecorationType({
    after: { color: new vscode.ThemeColor('editorCodeLens.foreground'), fontStyle: 'normal', margin: '0 0 0 1em' },
    rangeBehavior: vscode.DecorationRangeBehavior.ClosedClosed,
  });
  inlayChanged = new vscode.EventEmitter();
  inlayProvider.onDidChangeInlayHints = inlayChanged.event;
  context.subscriptions.push(output, diagnostics, decoration, inlayChanged);
  if (config().get('mode') !== 'hints') { activateLsp(context); return; }

  context.subscriptions.push(
    vscode.languages.registerInlayHintsProvider({ language: 'neant', scheme: 'file' }, inlayProvider),
    vscode.workspace.onDidOpenTextDocument(run),
    vscode.workspace.onDidSaveTextDocument((d) => { clearTimeout(timers.get(d.uri.toString())); timers.delete(d.uri.toString()); run(d); }),
    vscode.workspace.onDidCloseTextDocument((doc) => {
      results.delete(doc.uri.toString());
      diagnostics.delete(doc.uri);
    }),
    // an edit moves lines under the last run's hints; they come back, right, on the next save
    vscode.workspace.onDidChangeTextDocument((e) => {
      if (!isNeant(e.document) || e.contentChanges.length === 0) return;
      // the live buffer, once typing pauses; the save still runs on the file
      const ms = config().get('hints.debounceMs');
      if (ms > 0) {
        const key = e.document.uri.toString();
        clearTimeout(timers.get(key));
        timers.set(key, setTimeout(() => { timers.delete(key); run(e.document, true); }, ms));
        return;
      }
      // live hints off: an edit that moves lines clears the hints until the next save
      if (e.contentChanges.some((c) => c.range.start.line !== c.range.end.line || c.text.includes('\n'))) {
        results.delete(e.document.uri.toString());
        paint();
        inlayChanged.fire();
      }
    }),
    vscode.window.onDidChangeVisibleTextEditors(paint),
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (!e.affectsConfiguration('neant')) return;
      warnedMissing = false;
      vscode.workspace.textDocuments.forEach((d) => run(d));
    }),
    vscode.commands.registerCommand('neant.refreshHints', () => {
      const ed = vscode.window.activeTextEditor;
      if (ed) run(ed.document);
    }),
  );
  vscode.workspace.textDocuments.forEach((d) => run(d));
}

function deactivate() {
  stopServer();
  for (const t of timers.values()) clearTimeout(t);
  timers.clear();
  for (const child of running.values()) child.kill();
  running.clear();
}

module.exports = { activate, deactivate };
