//! `neant lsp`: the grey text as a Language Server, for any editor (editors/README.md). JSON-RPC
//! over stdio with `Content-Length` framing, full-text sync. Every answer is `neant hints` on the
//! buffer as the editor holds it — its document, parsed back — so nothing here knows anything
//! about costs: diagnostics are the document's `diagnostics`, an inlay hint is a function's `hint`
//! after its signature, a hover is its report, and a definition is where the module loader says
//! a function or a struct is. A buffer is analysed when it opens, changes or is saved, and a saved
//! file's dependents that are open are analysed again, since `use` reads from disk.

use crate::cost::Machine;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------- JSON, read and written

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    /// in the order written, so a response prints the same every time
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, k: &str) -> &Json {
        match self { Json::Obj(kv) => kv.iter().find(|(x, _)| x == k).map(|(_, v)| v).unwrap_or(&Json::Null), _ => &Json::Null }
    }
    fn at(&self, path: &[&str]) -> &Json { path.iter().fold(self, |j, k| j.get(k)) }
    fn str(&self) -> Option<&str> { if let Json::Str(s) = self { Some(s) } else { None } }
    fn num(&self) -> Option<f64> { if let Json::Num(n) = self { Some(*n) } else { None } }
    fn arr(&self) -> &[Json] { if let Json::Arr(a) = self { a } else { &[] } }
    fn line(&self) -> u32 { self.get("line").num().unwrap_or(0.0) as u32 }
}

fn obj(kv: Vec<(&str, Json)>) -> Json { Json::Obj(kv.into_iter().map(|(k, v)| (k.to_string(), v)).collect()) }
fn s(x: &str) -> Json { Json::Str(x.to_string()) }
fn n(x: u32) -> Json { Json::Num(x as f64) }

fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

impl std::fmt::Display for Json {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = String::new();
        fn go(j: &Json, out: &mut String) {
            match j {
                Json::Null => out.push_str("null"),
                Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
                Json::Num(x) if x.fract() == 0.0 && x.abs() < 1e15 => out.push_str(&format!("{}", *x as i64)),
                Json::Num(x) => out.push_str(&format!("{x}")),
                Json::Str(s) => write_str(out, s),
                Json::Arr(a) => {
                    out.push('[');
                    for (i, x) in a.iter().enumerate() { if i > 0 { out.push(','); } go(x, out); }
                    out.push(']');
                }
                Json::Obj(kv) => {
                    out.push('{');
                    for (i, (k, v)) in kv.iter().enumerate() { if i > 0 { out.push(','); } write_str(out, k); out.push(':'); go(v, out); }
                    out.push('}');
                }
            }
        }
        go(self, &mut out);
        f.write_str(&out)
    }
}

pub fn parse_json(src: &str) -> Result<Json, String> {
    let cs: Vec<char> = src.chars().collect();
    let mut i = 0;
    let v = value(&cs, &mut i)?;
    ws(&cs, &mut i);
    if i != cs.len() { return Err(format!("trailing text at {i}")); }
    Ok(v)
}

fn ws(cs: &[char], i: &mut usize) { while *i < cs.len() && cs[*i].is_whitespace() { *i += 1; } }

fn lit(cs: &[char], i: &mut usize, word: &str, v: Json) -> Result<Json, String> {
    let w: Vec<char> = word.chars().collect();
    if cs.len() >= *i + w.len() && cs[*i..*i + w.len()] == w[..] { *i += w.len(); Ok(v) } else { Err(format!("bad literal at {i}")) }
}

fn value(cs: &[char], i: &mut usize) -> Result<Json, String> {
    ws(cs, i);
    match cs.get(*i) {
        None => Err("unexpected end".into()),
        Some('n') => lit(cs, i, "null", Json::Null),
        Some('t') => lit(cs, i, "true", Json::Bool(true)),
        Some('f') => lit(cs, i, "false", Json::Bool(false)),
        Some('"') => string(cs, i).map(Json::Str),
        Some('[') => {
            *i += 1;
            let mut a = Vec::new();
            ws(cs, i);
            if cs.get(*i) == Some(&']') { *i += 1; return Ok(Json::Arr(a)); }
            loop {
                a.push(value(cs, i)?);
                ws(cs, i);
                match cs.get(*i) { Some(',') => *i += 1, Some(']') => { *i += 1; return Ok(Json::Arr(a)); } _ => return Err(format!("expected , or ] at {i}")) }
            }
        }
        Some('{') => {
            *i += 1;
            let mut kv = Vec::new();
            ws(cs, i);
            if cs.get(*i) == Some(&'}') { *i += 1; return Ok(Json::Obj(kv)); }
            loop {
                ws(cs, i);
                if cs.get(*i) != Some(&'"') { return Err(format!("expected a key at {i}")); }
                let k = string(cs, i)?;
                ws(cs, i);
                if cs.get(*i) != Some(&':') { return Err(format!("expected : at {i}")); }
                *i += 1;
                kv.push((k, value(cs, i)?));
                ws(cs, i);
                match cs.get(*i) { Some(',') => *i += 1, Some('}') => { *i += 1; return Ok(Json::Obj(kv)); } _ => return Err(format!("expected , or }} at {i}")) }
            }
        }
        Some(_) => {
            let start = *i;
            while *i < cs.len() && matches!(cs[*i], '-' | '+' | '.' | 'e' | 'E' | '0'..='9') { *i += 1; }
            let t: String = cs[start..*i].iter().collect();
            t.parse().map(Json::Num).map_err(|_| format!("bad value at {start}"))
        }
    }
}

fn string(cs: &[char], i: &mut usize) -> Result<String, String> {
    *i += 1;
    let mut out = String::new();
    let hex = |cs: &[char], at: usize| -> Result<u32, String> {
        let h: String = cs.get(at..at + 4).ok_or("short \\u escape")?.iter().collect();
        u32::from_str_radix(&h, 16).map_err(|_| "bad \\u escape".to_string())
    };
    loop {
        match cs.get(*i) {
            None => return Err("unterminated string".into()),
            Some('"') => { *i += 1; return Ok(out); }
            Some('\\') => {
                *i += 1;
                match cs.get(*i) {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('b') => out.push('\u{8}'),
                    Some('f') => out.push('\u{c}'),
                    Some('u') => {
                        let mut c = hex(cs, *i + 1)?;
                        *i += 4;
                        // a surrogate pair is one character
                        if (0xd800..0xdc00).contains(&c) && cs.get(*i + 1) == Some(&'\\') && cs.get(*i + 2) == Some(&'u') {
                            let lo = hex(cs, *i + 3)?;
                            if (0xdc00..0xe000).contains(&lo) { c = 0x10000 + ((c - 0xd800) << 10) + (lo - 0xdc00); *i += 6; }
                        }
                        out.push(char::from_u32(c).unwrap_or('\u{fffd}'));
                    }
                    Some(&c) => out.push(c),
                    None => return Err("unterminated escape".into()),
                }
                *i += 1;
            }
            Some(&c) => { out.push(c); *i += 1; }
        }
    }
}

// ---------------------------------------------------------------- URIs, lines, columns

fn uri_to_path(uri: &str) -> PathBuf {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let b = raw.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(x) = u8::from_str_radix(&raw[i + 1..i + 3], 16) { out.push(x); i += 3; continue; }
        }
        out.push(b[i]);
        i += 1;
    }
    PathBuf::from(String::from_utf8_lossy(&out).to_string())
}

fn path_to_uri(p: &Path) -> String {
    let mut out = String::from("file://");
    for &c in p.display().to_string().as_bytes() {
        if c.is_ascii_alphanumeric() || b"/-._~".contains(&c) { out.push(c as char); } else { out.push_str(&format!("%{c:02X}")); }
    }
    out
}

fn line_of(text: &str, l: u32) -> &str {
    text.split('\n').nth(l as usize).map(|x| x.strip_suffix('\r').unwrap_or(x)).unwrap_or("")
}

fn utf16_len(s: &str) -> u32 { s.chars().map(|c| c.len_utf16() as u32).sum() }

/// A 0-based char column as a UTF-16 one, the protocol's unit.
fn to_utf16(line: &str, col: usize) -> u32 { utf16_len(&line.chars().take(col).collect::<String>()) }

fn from_utf16(line: &str, u: u32) -> usize {
    let mut acc = 0;
    for (k, c) in line.chars().enumerate() { if acc >= u { return k; } acc += c.len_utf16() as u32; }
    line.chars().count()
}

fn is_word(c: char) -> bool { c.is_alphanumeric() || c == '_' }

/// The identifier under a 0-based char column, with its char range.
fn word_at(line: &str, col: usize) -> Option<(String, usize, usize)> {
    let cs: Vec<char> = line.chars().collect();
    let mut a = col.min(cs.len());
    if !(a < cs.len() && is_word(cs[a])) && a > 0 && is_word(cs[a - 1]) { a -= 1; }
    if a >= cs.len() || !is_word(cs[a]) { return None; }
    let mut b = a;
    while a > 0 && is_word(cs[a - 1]) { a -= 1; }
    while b < cs.len() && is_word(cs[b]) { b += 1; }
    if cs[a].is_ascii_digit() { return None; }
    Some((cs[a..b].iter().collect(), a, b))
}

/// Where `name` stands as a whole word on a line, as a char column.
fn find_word(line: &str, name: &str) -> Option<usize> {
    let cs: Vec<char> = line.chars().collect();
    let w: Vec<char> = name.chars().collect();
    (0..cs.len().saturating_sub(w.len() - 1)).find(|&k| {
        cs[k..].starts_with(&w) && (k == 0 || !is_word(cs[k - 1])) && cs.get(k + w.len()).is_none_or(|c| !is_word(*c))
    })
}

fn range(l: u32, a: u32, b: u32) -> Json {
    obj(vec![("start", obj(vec![("line", n(l)), ("character", n(a))])), ("end", obj(vec![("line", n(l)), ("character", n(b))]))])
}

// ---------------------------------------------------------------- the server

struct Server {
    machine: Machine,
    /// open buffers by URI
    docs: HashMap<String, String>,
    /// each open buffer's last `neant hints` document
    hints: HashMap<String, Json>,
    /// the URIs each root last published diagnostics to, so a fixed error is cleared
    published: HashMap<String, Vec<String>>,
    unknowns: bool,
    shut: bool,
    out: std::io::Stdout,
}

pub fn serve(machine: &Machine) -> i32 {
    let mut sv = Server { machine: machine.clone(), docs: HashMap::new(), hints: HashMap::new(), published: HashMap::new(), unknowns: false, shut: false, out: std::io::stdout() };
    let mut input = std::io::BufReader::new(std::io::stdin());
    loop {
        let Some(body) = read_message(&mut input) else { return 1 };
        let msg = match parse_json(&body) {
            Ok(m) => m,
            Err(e) => { sv.send(obj(vec![("jsonrpc", s("2.0")), ("id", Json::Null), ("error", obj(vec![("code", Json::Num(-32700.0)), ("message", s(&e))]))])); continue; }
        };
        let method = msg.get("method").str().unwrap_or("").to_string();
        if method == "exit" { return if sv.shut { 0 } else { 1 }; }
        let id = msg.get("id").clone();
        let answer = sv.handle(&method, msg.get("params"));
        if id == Json::Null { continue; }
        let reply = match answer {
            Some(result) => obj(vec![("jsonrpc", s("2.0")), ("id", id), ("result", result)]),
            None => obj(vec![("jsonrpc", s("2.0")), ("id", id), ("error", obj(vec![("code", Json::Num(-32601.0)), ("message", s(&format!("unhandled method {method}")))]))]),
        };
        sv.send(reply);
    }
}

fn read_message(r: &mut impl BufRead) -> Option<String> {
    let mut len: Option<usize> = None;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).ok()? == 0 { return None; }
        let h = h.trim_end();
        if h.is_empty() { if len.is_some() { break; } continue; }
        if let Some((k, v)) = h.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") { len = v.trim().parse().ok(); }
        }
    }
    let mut buf = vec![0; len?];
    r.read_exact(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).to_string())
}

impl Server {
    fn send(&mut self, j: Json) {
        let body = j.to_string();
        let _ = write!(self.out, "Content-Length: {}\r\n\r\n{body}", body.len());
        let _ = self.out.flush();
    }

    fn notify(&mut self, method: &str, params: Json) {
        self.send(obj(vec![("jsonrpc", s("2.0")), ("method", s(method)), ("params", params)]));
    }

    /// `Some(result)` answers a request; `None` is a method this server does not have.
    fn handle(&mut self, method: &str, p: &Json) -> Option<Json> {
        let uri = p.at(&["textDocument", "uri"]).str().unwrap_or("").to_string();
        match method {
            "initialize" => {
                self.unknowns = p.at(&["initializationOptions", "unknownsAsDiagnostics"]) == &Json::Bool(true);
                Some(obj(vec![
                    ("capabilities", obj(vec![
                        ("textDocumentSync", obj(vec![("openClose", Json::Bool(true)), ("change", n(1)), ("save", obj(vec![("includeText", Json::Bool(false))]))])),
                        ("hoverProvider", Json::Bool(true)),
                        ("definitionProvider", Json::Bool(true)),
                        ("inlayHintProvider", Json::Bool(true)),
                    ])),
                    ("serverInfo", obj(vec![("name", s("neant")), ("version", s(env!("CARGO_PKG_VERSION")))])),
                ]))
            }
            "initialized" | "$/cancelRequest" | "$/setTrace" | "workspace/didChangeConfiguration" => Some(Json::Null),
            "shutdown" => { self.shut = true; Some(Json::Null) }
            "textDocument/didOpen" => {
                let text = p.at(&["textDocument", "text"]).str().unwrap_or("").to_string();
                self.docs.insert(uri.clone(), text);
                self.analyze(&uri);
                Some(Json::Null)
            }
            "textDocument/didChange" => {
                // full sync: the last change is the whole buffer
                if let Some(t) = p.get("contentChanges").arr().last().and_then(|c| c.get("text").str()) {
                    self.docs.insert(uri.clone(), t.to_string());
                    self.analyze(&uri);
                }
                Some(Json::Null)
            }
            "textDocument/didSave" => {
                self.analyze(&uri);
                // an open file that `use`s the saved one read it from disk: read it again
                let saved = uri_to_path(&uri);
                let dependents: Vec<String> = self.hints.iter()
                    .filter(|(u, h)| **u != uri && (h.get("functions").arr().is_empty() || self.names_of(u, h).contains(&saved)))
                    .map(|(u, _)| u.clone()).collect();
                for u in dependents { self.analyze(&u); }
                Some(Json::Null)
            }
            "textDocument/didClose" => {
                self.docs.remove(&uri);
                self.hints.remove(&uri);
                for u in self.published.remove(&uri).unwrap_or_default() {
                    self.notify("textDocument/publishDiagnostics", obj(vec![("uri", s(&u)), ("diagnostics", Json::Arr(vec![]))]));
                }
                Some(Json::Null)
            }
            "textDocument/inlayHint" => Some(self.inlay(&uri, p)),
            "textDocument/hover" => Some(self.hover(&uri, p)),
            "textDocument/definition" => Some(self.definition(&uri, p)),
            m if m.starts_with("$/") => Some(Json::Null),
            _ => None,
        }
    }

    /// A path the hints document names, as a file: absolute (the root is), or `std/…`.
    fn resolve(&self, name: &str) -> PathBuf {
        let p = match name.strip_prefix("std/") { Some(r) if !Path::new(name).exists() => crate::modules::std_root().join(r), _ => PathBuf::from(name) };
        // `dir/../x.nt` as the loader joined it is `x.nt` to the editor
        let mut out = PathBuf::new();
        for c in p.components() {
            match c { std::path::Component::CurDir => {}, std::path::Component::ParentDir if out.file_name().is_some() => { out.pop(); } c => out.push(c) }
        }
        out
    }

    /// The file a function or diagnostic of `uri`'s document is in.
    fn file_of(&self, uri: &str, x: &Json) -> PathBuf {
        match x.get("file").str() { Some(f) => self.resolve(f), None => uri_to_path(uri) }
    }

    fn names_of(&self, uri: &str, h: &Json) -> Vec<PathBuf> {
        h.get("functions").arr().iter().chain(h.get("diagnostics").arr()).map(|x| self.file_of(uri, x)).collect()
    }

    /// A file's text: its buffer when it is open, else what is on disk.
    fn text_of(&self, p: &Path) -> String {
        self.docs.get(&path_to_uri(p)).cloned().unwrap_or_else(|| std::fs::read_to_string(p).unwrap_or_default())
    }

    fn analyze(&mut self, uri: &str) {
        let Some(text) = self.docs.get(uri).cloned() else { return };
        let path = uri_to_path(uri);
        let doc = parse_json(&crate::hints::run(&path, &text, &self.machine)).unwrap_or(Json::Null);
        // diagnostics by file, the root's first; `use`d files are read from disk, and numbered so
        let mut by: Vec<(String, Vec<Json>)> = vec![(uri.to_string(), vec![])];
        let put = |by: &mut Vec<(String, Vec<Json>)>, u: String, d: Json| match by.iter_mut().find(|(x, _)| *x == u) {
            Some((_, v)) => v.push(d),
            None => by.push((u, vec![d])),
        };
        for d in doc.get("diagnostics").arr() {
            let f = self.file_of(uri, d);
            let t = if f == path { text.clone() } else { std::fs::read_to_string(&f).unwrap_or_default() };
            let l = d.line().saturating_sub(1);
            let line = line_of(&t, l);
            let c = (d.get("col").num().unwrap_or(1.0) as usize).saturating_sub(1).min(line.chars().count());
            let (a, b) = match word_at(line, c) { Some((_, a, b)) if a == c => (a, b), _ => (c, line.chars().count()) };
            let sev = if d.get("severity").str() == Some("warning") { 2 } else { 1 };
            let msg = d.get("message").str().unwrap_or("").to_string();
            put(&mut by, path_to_uri(&f), obj(vec![("range", range(l, to_utf16(line, a), to_utf16(line, b))), ("severity", n(sev)), ("source", s("neant")), ("message", s(&msg))]));
        }
        if self.unknowns {
            for f in doc.get("functions").arr() {
                let (Some(cause), Some(cl)) = (f.get("cause").str(), f.get("cause_line").num()) else { continue };
                let file = self.file_of(uri, f);
                let t = if file == path { text.clone() } else { std::fs::read_to_string(&file).unwrap_or_default() };
                let l = (cl as u32).saturating_sub(1);
                let line = line_of(&t, l);
                let a = line.chars().take_while(|c| c.is_whitespace()).count();
                let msg = format!("`{}`'s cost is unknown: {cause}", f.get("name").str().unwrap_or(""));
                put(&mut by, path_to_uri(&file), obj(vec![("range", range(l, to_utf16(line, a), utf16_len(line))), ("severity", n(4)), ("source", s("neant cost")), ("message", s(&msg))]));
            }
        }
        let before = self.published.remove(uri).unwrap_or_default();
        for u in before.iter().filter(|u| !by.iter().any(|(x, _)| x == *u)) {
            self.notify("textDocument/publishDiagnostics", obj(vec![("uri", s(u)), ("diagnostics", Json::Arr(vec![]))]));
        }
        let now: Vec<String> = by.iter().map(|(u, _)| u.clone()).collect();
        for (u, ds) in by { self.notify("textDocument/publishDiagnostics", obj(vec![("uri", s(&u)), ("diagnostics", Json::Arr(ds))])); }
        self.published.insert(uri.to_string(), now);
        self.hints.insert(uri.to_string(), doc);
    }

    /// The functions of the last analysis whose `fn` is in this document.
    fn here(&self, uri: &str) -> Vec<Json> {
        let Some(h) = self.hints.get(uri) else { return vec![] };
        let me = uri_to_path(uri);
        h.get("functions").arr().iter().filter(|f| self.file_of(uri, f) == me).cloned().collect()
    }

    fn inlay(&self, uri: &str, p: &Json) -> Json {
        let text = self.docs.get(uri).cloned().unwrap_or_default();
        let from = p.at(&["range", "start", "line"]).num().unwrap_or(0.0) as u32;
        let to = p.at(&["range", "end", "line"]).num().unwrap_or(f64::MAX) as u32;
        Json::Arr(self.here(uri).iter().filter(|f| f.line() >= 1 && (from..=to).contains(&(f.line() - 1))).map(|f| {
            let l = f.line() - 1;
            obj(vec![
                ("position", obj(vec![("line", n(l)), ("character", n(utf16_len(line_of(&text, l))))])),
                ("label", s(f.get("hint").str().unwrap_or(""))),
                ("paddingLeft", Json::Bool(true)),
            ])
        }).collect())
    }

    /// The identifier under the request's position, and the line it is on.
    fn word(&self, uri: &str, p: &Json) -> Option<(String, u32, u32, u32)> {
        let text = self.docs.get(uri)?;
        let l = p.at(&["position", "line"]).num()? as u32;
        let line = line_of(text, l);
        let (w, a, b) = word_at(line, from_utf16(line, p.at(&["position", "character"]).num()? as u32))?;
        Some((w, l, to_utf16(line, a), to_utf16(line, b)))
    }

    /// The function's line and everything the report prints under it, as the VS Code hover was.
    fn hover(&self, uri: &str, p: &Json) -> Json {
        let Some((w, l, a, b)) = self.word(uri, p) else { return Json::Null };
        let Some(f) = self.hints.get(uri).and_then(|h| h.get("functions").arr().iter().find(|f| f.get("name").str() == Some(&w)).cloned()) else { return Json::Null };
        let g = |k: &str| f.get(k).str().unwrap_or("").to_string();
        let mut lines = vec![format!("{}  {}", g("name"), g("hint"))];
        if let Some(work) = f.get("work").str() {
            if g("hint") != format!("work {work}   moves {}   {}", g("moves"), g("tier")) {
                lines.push(format!("work   {work}"));
                lines.push(format!("moves  {}", g("moves")));
            }
        }
        if let Some(sp) = f.get("span").str() { lines.push(format!("span   {sp}")); }
        for r in f.get("report").arr() { lines.push(r.str().unwrap_or("").to_string()); }
        obj(vec![
            ("contents", obj(vec![("kind", s("markdown")), ("value", s(&format!("```text\n{}\n```", lines.join("\n"))))])),
            ("range", range(l, a, b)),
        ])
    }

    /// A function or a struct name to where it is defined, in whichever file of the program.
    fn definition(&self, uri: &str, p: &Json) -> Json {
        let Some((w, ..)) = self.word(uri, p) else { return Json::Null };
        let path = uri_to_path(uri);
        let text = self.docs.get(uri).cloned().unwrap_or_default();
        let Ok((prog, sources)) = crate::modules::load(&path, &text) else { return Json::Null };
        let at = prog.funcs.iter().find(|f| f.name == w).map(|f| f.line)
            .or_else(|| prog.structs.iter().find(|st| st.name == w).map(|st| st.line));
        let Some((file, l)) = at.and_then(|g| sources.place(g)).map(|(f, l)| (self.resolve(f), l)) else { return Json::Null };
        let t = if file == path { text } else { self.text_of(&file) };
        let l = l.saturating_sub(1);
        let line = line_of(&t, l);
        let c = find_word(line, &w).unwrap_or(0);
        obj(vec![("uri", s(&path_to_uri(&file))), ("range", range(l, to_utf16(line, c), to_utf16(line, c + w.chars().count())))])
    }
}
