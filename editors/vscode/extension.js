// The editor surface of neant: on open and on save, run `neant hints` on the file and show each
// function's cost as grey text after its `fn` line, the report under it in a hover, and every
// error in the Problems panel. Nothing here computes a cost; it prints what the compiler said.
// Plain JavaScript against the vscode API, no build step and no dependencies.

'use strict';

const vscode = require('vscode');
const cp = require('child_process');
const path = require('path');

/** Results by document URI: the parsed `neant hints` document of the last run. */
const results = new Map();
/** Runs in flight by URI, so a save during a run kills the stale one. */
const running = new Map();

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

function run(doc) {
  if (!isNeant(doc)) return;
  const key = doc.uri.toString();
  const prev = running.get(key);
  if (prev) prev.kill();
  const args = ['hints', ...(config().get('args') || []), doc.uri.fsPath];
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
  running.set(key, child);
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
  for (const d of hints.diagnostics || []) {
    const sev = d.severity === 'warning' ? vscode.DiagnosticSeverity.Warning : vscode.DiagnosticSeverity.Error;
    const diag = new vscode.Diagnostic(span(doc, at(doc, d.line, d.col)), d.message, sev);
    diag.source = 'neant';
    list.push(diag);
  }
  if (config().get('unknownsAsDiagnostics')) {
    for (const f of hints.functions || []) {
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
    const opts = (hints.functions || []).filter((f) => f.line >= 1 && f.line <= doc.lineCount).map((f) => {
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
    return (hints.functions || [])
      .filter((f) => f.line >= 1 && f.line <= doc.lineCount && f.line - 1 >= range.start.line && f.line - 1 <= range.end.line)
      .map((f) => {
        const h = new vscode.InlayHint(doc.lineAt(f.line - 1).range.end, `// ${clip(f.hint)}`);
        h.paddingLeft = true;
        h.tooltip = hover(f);
        return h;
      });
  },
};

function activate(context) {
  output = vscode.window.createOutputChannel('neant');
  diagnostics = vscode.languages.createDiagnosticCollection('neant');
  decoration = vscode.window.createTextEditorDecorationType({
    after: { color: new vscode.ThemeColor('editorCodeLens.foreground'), fontStyle: 'normal', margin: '0 0 0 1em' },
    rangeBehavior: vscode.DecorationRangeBehavior.ClosedClosed,
  });
  inlayChanged = new vscode.EventEmitter();
  inlayProvider.onDidChangeInlayHints = inlayChanged.event;

  context.subscriptions.push(
    output, diagnostics, decoration, inlayChanged,
    vscode.languages.registerInlayHintsProvider({ language: 'neant', scheme: 'file' }, inlayProvider),
    vscode.workspace.onDidOpenTextDocument(run),
    vscode.workspace.onDidSaveTextDocument(run),
    vscode.workspace.onDidCloseTextDocument((doc) => {
      results.delete(doc.uri.toString());
      diagnostics.delete(doc.uri);
    }),
    // an edit moves lines under the last run's hints; they come back, right, on the next save
    vscode.workspace.onDidChangeTextDocument((e) => {
      if (!isNeant(e.document) || e.contentChanges.length === 0) return;
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
      vscode.workspace.textDocuments.forEach(run);
    }),
    vscode.commands.registerCommand('neant.refreshHints', () => {
      const ed = vscode.window.activeTextEditor;
      if (ed) run(ed.document);
    }),
  );
  vscode.workspace.textDocuments.forEach(run);
}

function deactivate() {
  for (const child of running.values()) child.kill();
  running.clear();
}

module.exports = { activate, deactivate };
