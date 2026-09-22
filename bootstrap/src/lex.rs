//! Tokens. `//` comments to end of line. A float literal has a `.` followed by a digit, so
//! `0..n` is Int DotDot Ident and `a.len()` is Ident Dot Ident.

use crate::diag::{err, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(String),
    Int(i64),
    Float(f64),
    /// `b'a'`
    Byte(u8),
    /// `b"..."`
    Bytes(Vec<u8>),
    /// `"..."` — only inside attributes for now
    Str(String),
    Fn, Let, Mut, If, Else, For, In, Return, True, False, As, While, Break, Decreasing,
    LParen, RParen, LBracket, RBracket, LBrace, RBrace,
    Comma, Semi, Colon, Arrow, Dot, DotDot,
    Eq, EqEq, Ne, Lt, Le, Gt, Ge,
    Plus, Minus, Star, Slash, Percent,
    PlusEq, MinusEq, StarEq, SlashEq,
    Amp, AmpAmp, Pipe, PipePipe, Bang, Hash,
    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub line: u32,
    pub col: u32,
}

pub fn lex(src: &str) -> Result<Vec<Token>> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let (mut i, mut line, mut col) = (0usize, 1u32, 1u32);

    macro_rules! push {
        ($tok:expr, $l:expr, $c:expr) => {
            out.push(Token { tok: $tok, line: $l, col: $c })
        };
    }

    while i < chars.len() {
        let c = chars[i];
        let (l, cl) = (line, col);
        // whitespace
        if c == '\n' {
            i += 1; line += 1; col = 1; continue;
        }
        if c.is_whitespace() {
            i += 1; col += 1; continue;
        }
        // comment
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' { i += 1; }
            continue;
        }
        // byte and byte-string literals: b'x', b"..."
        if c == 'b' && matches!(chars.get(i + 1), Some('\'') | Some('"')) {
            let quote = chars[i + 1];
            let mut j = i + 2;
            let mut bytes = Vec::new();
            loop {
                let Some(&ch) = chars.get(j) else { return err(l, cl, "unterminated byte literal") };
                if ch == quote { break; }
                if ch == '\n' { return err(l, cl, "byte literal runs past the end of the line"); }
                if ch == '\\' {
                    let Some(&esc) = chars.get(j + 1) else { return err(l, cl, "unterminated escape") };
                    bytes.push(match esc {
                        'n' => b'\n', 't' => b'\t', 'r' => b'\r', '0' => 0, '\\' => b'\\', '\'' => b'\'', '"' => b'"',
                        other => return err(l, cl, format!("unknown escape `\\{other}`")),
                    });
                    j += 2;
                    continue;
                }
                if !ch.is_ascii() { return err(l, cl, "byte literals hold ASCII only"); }
                bytes.push(ch as u8);
                j += 1;
            }
            let tok = if quote == '\'' {
                if bytes.len() != 1 { return err(l, cl, "a byte literal `b'..'` holds exactly one byte"); }
                Tok::Byte(bytes[0])
            } else { Tok::Bytes(bytes) };
            push!(tok, l, cl);
            col += (j + 1 - i) as u32;
            i = j + 1;
            continue;
        }
        // string literal (attributes)
        if c == '"' {
            let mut j = i + 1;
            let mut s = String::new();
            loop {
                let Some(&ch) = chars.get(j) else { return err(l, cl, "unterminated string") };
                if ch == '"' { break; }
                if ch == '\n' { return err(l, cl, "string runs past the end of the line"); }
                s.push(ch);
                j += 1;
            }
            push!(Tok::Str(s), l, cl);
            col += (j + 1 - i) as u32;
            i = j + 1;
            continue;
        }
        // identifier / keyword
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') { i += 1; }
            let word: String = chars[start..i].iter().collect();
            col += (i - start) as u32;
            let tok = match word.as_str() {
                "fn" => Tok::Fn, "let" => Tok::Let, "mut" => Tok::Mut, "if" => Tok::If,
                "else" => Tok::Else, "for" => Tok::For, "in" => Tok::In, "return" => Tok::Return,
                "true" => Tok::True, "false" => Tok::False, "as" => Tok::As,
                "while" => Tok::While, "break" => Tok::Break, "decreasing" => Tok::Decreasing,
                _ => Tok::Ident(word),
            };
            push!(tok, l, cl);
            continue;
        }
        // number
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '_') { i += 1; }
            let mut is_float = false;
            if i + 1 < chars.len() && chars[i] == '.' && chars[i + 1].is_ascii_digit() {
                is_float = true;
                i += 1;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '_') { i += 1; }
            }
            if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                let mut j = i + 1;
                if j < chars.len() && (chars[j] == '+' || chars[j] == '-') { j += 1; }
                if j < chars.len() && chars[j].is_ascii_digit() {
                    is_float = true;
                    i = j;
                    while i < chars.len() && chars[i].is_ascii_digit() { i += 1; }
                }
            }
            let text: String = chars[start..i].iter().filter(|c| **c != '_').collect();
            col += (i - start) as u32;
            if is_float {
                match text.parse::<f64>() {
                    Ok(v) => push!(Tok::Float(v), l, cl),
                    Err(_) => return err(l, cl, format!("bad float literal `{text}`")),
                }
            } else {
                match text.parse::<i64>() {
                    Ok(v) => push!(Tok::Int(v), l, cl),
                    Err(_) => return err(l, cl, format!("integer literal `{text}` does not fit in i64")),
                }
            }
            continue;
        }
        // punctuation
        let two = |a: char, b: char| c == a && chars.get(i + 1) == Some(&b);
        let (tok, n) = if two('-', '>') { (Tok::Arrow, 2) }
            else if two('.', '.') { (Tok::DotDot, 2) }
            else if two('=', '=') { (Tok::EqEq, 2) }
            else if two('!', '=') { (Tok::Ne, 2) }
            else if two('<', '=') { (Tok::Le, 2) }
            else if two('>', '=') { (Tok::Ge, 2) }
            else if two('+', '=') { (Tok::PlusEq, 2) }
            else if two('-', '=') { (Tok::MinusEq, 2) }
            else if two('*', '=') { (Tok::StarEq, 2) }
            else if two('/', '=') { (Tok::SlashEq, 2) }
            else if two('&', '&') { (Tok::AmpAmp, 2) }
            else if two('|', '|') { (Tok::PipePipe, 2) }
            else {
                let t = match c {
                    '(' => Tok::LParen, ')' => Tok::RParen, '[' => Tok::LBracket, ']' => Tok::RBracket,
                    '{' => Tok::LBrace, '}' => Tok::RBrace, ',' => Tok::Comma, ';' => Tok::Semi,
                    ':' => Tok::Colon, '.' => Tok::Dot, '=' => Tok::Eq, '<' => Tok::Lt, '>' => Tok::Gt,
                    '+' => Tok::Plus, '-' => Tok::Minus, '*' => Tok::Star, '/' => Tok::Slash,
                    '%' => Tok::Percent, '&' => Tok::Amp, '!' => Tok::Bang, '|' => Tok::Pipe, '#' => Tok::Hash,
                    _ => return err(l, cl, format!("unexpected character `{c}`")),
                };
                (t, 1)
            };
        push!(tok, l, cl);
        i += n; col += n as u32;
    }
    push!(Tok::Eof, line, col);
    Ok(out)
}
