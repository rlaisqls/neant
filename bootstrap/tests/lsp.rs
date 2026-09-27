//! `neant lsp` pinned: a scripted session over stdio on tests/lsp's two-file program — initialize,
//! didOpen of main.nt (which `use`s geometry.nt), inlay hints, a hover on a function of each file,
//! a definition of a function and of a struct in the other file, a didChange that breaks a call's
//! arity and gets its diagnostic, one that mends it and clears it, a second buffer whose `use`d
//! file has a type error (published to that file) closed again, shutdown and exit. Every message
//! the server sends, one per line with the repository's path as `$ROOT`, must be
//! tests/lsp/session.out.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn q(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn frame(body: &str) -> String { format!("Content-Length: {}\r\n\r\n{body}", body.len()) }

#[test]
fn lsp_session() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").canonicalize().unwrap();
    let root_s = root.display().to_string();
    let uri = format!("file://{root_s}/tests/lsp/main.nt");
    let text = std::fs::read_to_string(root.join("tests/lsp/main.nt")).unwrap();
    let other = format!("file://{root_s}/tests/lsp/uses_broken.nt");
    let other_text = std::fs::read_to_string(root.join("tests/lsp/uses_broken.nt")).unwrap();
    let broken = text.replace("dot(ps[i], ps[i])", "dot(ps[i])");
    assert_ne!(broken, text);
    let doc = format!("{{\"uri\":{}}}", q(&uri));
    let at = |id: u32, method: &str, line: u32, ch: u32| format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"{method}\",\"params\":{{\"textDocument\":{doc},\"position\":{{\"line\":{line},\"character\":{ch}}}}}}}");
    let change = |v: u32, t: &str| format!(
        "{{\"jsonrpc\":\"2.0\",\"method\":\"textDocument/didChange\",\"params\":{{\"textDocument\":{{\"uri\":{},\"version\":{v}}},\"contentChanges\":[{{\"text\":{}}}]}}}}", q(&uri), q(t));
    let script = [
        format!("{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{{\"processId\":null,\"rootUri\":{},\"capabilities\":{{}}}}}}", q(&format!("file://{root_s}"))),
        "{\"jsonrpc\":\"2.0\",\"method\":\"initialized\",\"params\":{}}".to_string(),
        format!("{{\"jsonrpc\":\"2.0\",\"method\":\"textDocument/didOpen\",\"params\":{{\"textDocument\":{{\"uri\":{},\"languageId\":\"neant\",\"version\":1,\"text\":{}}}}}}}", q(&uri), q(&text)),
        format!("{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"textDocument/inlayHint\",\"params\":{{\"textDocument\":{doc},\"range\":{{\"start\":{{\"line\":0,\"character\":0}},\"end\":{{\"line\":20,\"character\":0}}}}}}}}"),
        // `norms` in main's call, `dot` in norms' body, `Point` in main's literal
        at(3, "textDocument/hover", 12, 13),
        at(4, "textDocument/hover", 5, 17),
        at(5, "textDocument/definition", 5, 16),
        at(6, "textDocument/definition", 11, 18),
        at(7, "textDocument/definition", 12, 12),
        change(2, &broken),
        change(3, &text),
        // a buffer whose `use`d file has the error: its diagnostic goes to that file, and closing
        // the buffer clears both
        format!("{{\"jsonrpc\":\"2.0\",\"method\":\"textDocument/didOpen\",\"params\":{{\"textDocument\":{{\"uri\":{},\"languageId\":\"neant\",\"version\":1,\"text\":{}}}}}}}", q(&other), q(&other_text)),
        format!("{{\"jsonrpc\":\"2.0\",\"method\":\"textDocument/didClose\",\"params\":{{\"textDocument\":{{\"uri\":{}}}}}}}", q(&other)),
        "{\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"shutdown\",\"params\":null}".to_string(),
        "{\"jsonrpc\":\"2.0\",\"method\":\"exit\",\"params\":null}".to_string(),
    ];
    let mut child = Command::new(env!("CARGO_BIN_EXE_neant")).arg("lsp").current_dir(&root)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(script.iter().map(|m| frame(m)).collect::<String>().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "exit after shutdown is 0");
    // take the framing back apart: every message's body, one per line
    let raw = String::from_utf8(out.stdout).unwrap();
    let mut got = String::new();
    let mut rest = raw.as_str();
    while let Some(i) = rest.find("\r\n\r\n") {
        let len: usize = rest[..i].trim().strip_prefix("Content-Length: ").expect("a Content-Length header").parse().unwrap();
        let body = &rest[i + 4..i + 4 + len];
        got.push_str(&body.replace(&root_s, "$ROOT"));
        got.push('\n');
        rest = &rest[i + 4 + len..];
    }
    assert!(rest.is_empty(), "trailing output: {rest:?}");
    let want = std::fs::read_to_string(root.join("tests/lsp/session.out")).unwrap_or_default();
    if got != want { panic!("the session differs\n--- got ---\n{got}--- want ---\n{want}"); }
}
