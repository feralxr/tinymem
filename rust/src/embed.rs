//! Tokenisation and the 1024-wide int8 hashing embedder (see docs/FORMAT.md).

use std::collections::HashMap;
use unicode_general_category::{get_general_category, GeneralCategory as GC};

pub const DIM: usize = 1024;
pub const EMBEDDER: &str = "hash1024-v1";

const STOP: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "has", "have", "in", "is", "it", "its", "of", "on",
    "or", "that", "the", "this", "to", "was", "were", "will", "with",
];

pub fn is_stop(w: &str) -> bool {
    STOP.binary_search(&w).is_ok()
}

fn is_word_char(c: char) -> bool {
    matches!(
        get_general_category(c),
        GC::UppercaseLetter
            | GC::LowercaseLetter
            | GC::TitlecaseLetter
            | GC::ModifierLetter
            | GC::OtherLetter
            | GC::DecimalNumber
            | GC::LetterNumber
            | GC::OtherNumber
    )
}

/// Maximal runs of letters/digits (underscore and punctuation separate words).
pub fn words(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        if is_word_char(c) {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(s) = start.take() {
            out.push(&text[s..i]);
        }
    }
    if let Some(s) = start {
        out.push(&text[s..]);
    }
    out
}

/// Split an identifier on camelCase / acronym boundaries, lower-cased.
pub fn split_word(w: &str) -> Vec<String> {
    if !w.chars().any(|c| c.is_uppercase()) {
        return vec![w.to_lowercase()];
    }
    let chars: Vec<(usize, char)> = w.char_indices().collect();
    let n = chars.len();
    #[derive(PartialEq, Clone, Copy)]
    enum Cls {
        U,
        L,
        O,
    }
    let cls: Vec<Cls> = chars
        .iter()
        .map(|&(_, c)| {
            if c.is_uppercase() {
                Cls::U
            } else if c.is_lowercase() || get_general_category(c) == GC::DecimalNumber {
                Cls::L
            } else {
                Cls::O
            }
        })
        .collect();
    let mut parts = Vec::new();
    let mut start = 0usize;
    for i in 1..n {
        if cls[i] == Cls::U && (cls[i - 1] == Cls::L || (cls[i - 1] == Cls::U && i + 1 < n && cls[i + 1] == Cls::L)) {
            parts.push(w[chars[start].0..chars[i].0].to_lowercase());
            start = i;
        }
    }
    parts.push(w[chars[start].0..].to_lowercase());
    parts
}

/// Sub-words of every multi-part identifier, space separated (FTS `terms` column).
pub fn terms_of(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for w in words(text) {
        let parts = split_word(w);
        if parts.len() > 1 {
            out.extend(parts);
        }
    }
    out.join(" ")
}

pub fn features(text: &str) -> HashMap<String, u32> {
    let mut counts: HashMap<String, u32> = HashMap::new();
    let mut seq: Vec<String> = Vec::new();
    for w in words(text) {
        let parts = split_word(w);
        if parts.len() > 1 {
            *counts.entry(w.to_lowercase()).or_insert(0) += 1;
        }
        for p in parts {
            let len = p.chars().count();
            if len < 2 || is_stop(&p) {
                continue;
            }
            *counts.entry(p.clone()).or_insert(0) += 1;
            if len >= 6 {
                let key: String = std::iter::once('~').chain(p.chars().take(5)).collect();
                *counts.entry(key).or_insert(0) += 1;
            }
            seq.push(p);
        }
    }
    for pair in seq.windows(2) {
        let key = format!("{} {}", pair[0], pair[1]);
        *counts.entry(key).or_insert(0) += 1;
    }
    counts
}

fn fnv1a64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &c in data {
        h ^= c as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn weight(key: &str) -> f64 {
    if key.starts_with('~') {
        0.4
    } else if key.contains(' ') {
        0.5
    } else {
        1.0
    }
}

pub fn embed(text: &str) -> Vec<i8> {
    let feats = features(text);
    let mut keys: Vec<&String> = feats.keys().collect();
    keys.sort();
    let mut vec = vec![0f64; DIM];
    for key in keys {
        let count = feats[key];
        let h = fnv1a64(key.as_bytes());
        let sign = if (h >> 32) & 1 == 0 { 1.0 } else { -1.0 };
        vec[(h % DIM as u64) as usize] += sign * weight(key) * (1.0 + (count as f64).ln());
    }
    let mut norm2 = 0.0f64;
    for v in &vec {
        norm2 += v * v;
    }
    let norm = norm2.sqrt();
    if norm == 0.0 {
        return vec![0; DIM];
    }
    vec.iter()
        .map(|v| {
            let q = (v / norm * 127.0 + 0.5).floor() as i32;
            q.clamp(-127, 127) as i8
        })
        .collect()
}

pub fn embed_blob(text: &str) -> Vec<u8> {
    embed(text).into_iter().map(|b| b as u8).collect()
}

fn is_digit(c: char) -> bool {
    c.is_ascii_digit()
}

/// 'sha256' -> ["sha", "256"]; words that are all letters or all digits yield nothing.
fn alpha_digit_parts(w: &str) -> Vec<String> {
    let mut runs: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in w.chars() {
        if let Some(last) = cur.chars().last() {
            if is_digit(c) != is_digit(last) {
                runs.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        runs.push(cur);
    }
    if runs.len() > 1 {
        runs
    } else {
        Vec::new()
    }
}

/// Build the FTS5 MATCH expression: each term (and identifier sub-words) OR-ed and quoted.
pub fn fts_query(q: &str) -> String {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut terms: Vec<String> = Vec::new();
    let mut add = |t: String| {
        if t.chars().count() >= 2 && seen.insert(t.clone()) {
            terms.push(t);
        }
    };
    let ws = words(q);
    for w in &ws {
        let parts = split_word(w);
        add(w.to_lowercase());
        if parts.len() > 1 {
            for t in parts {
                add(t);
            }
        }
        for t in alpha_digit_parts(&w.to_lowercase()) {
            add(t);
        }
    }
    for pair in ws.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if !a.is_empty() && a.chars().all(char::is_alphabetic) && !b.is_empty() && b.chars().all(is_digit) {
            add(format!("{}{}", a.to_lowercase(), b));
        }
    }
    let non_stop: Vec<String> = terms.iter().filter(|t| !is_stop(t)).cloned().collect();
    let terms = if non_stop.is_empty() { terms } else { non_stop };
    terms
        .iter()
        .take(32)
        .map(|t| format!("\"{}\"", t))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// Lower-cased query words used to choose the snippet window.
pub fn query_terms(q: &str) -> Vec<String> {
    let low = q.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    for w in words(&low) {
        if w.chars().count() >= 2 && !is_stop(w) && !out.iter().any(|x| x == w) {
            out.push(w.to_string());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Symbol extraction (declared names) and the path prior
// ---------------------------------------------------------------------------

const DECL: &[&str] = &[
    "func",
    "fn",
    "def",
    "class",
    "struct",
    "enum",
    "trait",
    "interface",
    "type",
    "mod",
    "module",
    "const",
    "static",
    "union",
    "namespace",
    "protocol",
    "impl",
    "macro_rules",
    "record",
    "object",
    "function",
];
const MODIFIERS: &[&str] = &[
    "pub",
    "crate",
    "super",
    "in",
    "self",
    "async",
    "unsafe",
    "extern",
    "export",
    "default",
    "abstract",
    "final",
    "public",
    "private",
    "protected",
    "internal",
    "static",
    "override",
    "virtual",
    "sealed",
    "partial",
    "inline",
    "declare",
    "readonly",
    "open",
    "data",
    "C",
];
pub const MAX_SYMBOLS: usize = 64;

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// ASCII identifier runs of `line` as (token, start, end) byte offsets.
fn ident_tokens(line: &str) -> Vec<(&str, usize, usize)> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if is_ident_byte(bytes[i]) {
            let mut j = i + 1;
            while j < bytes.len() && is_ident_byte(bytes[j]) {
                j += 1;
            }
            out.push((&line[i..j], i, j));
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// If `line[pos..]` (after spaces) starts with `open`, return the index after its matching `close`, else `pos`.
fn skip_group(line: &str, mut pos: usize, open: u8, close: u8) -> usize {
    let bytes = line.as_bytes();
    let n = bytes.len();
    while pos < n && bytes[pos] == b' ' {
        pos += 1;
    }
    if pos >= n || bytes[pos] != open {
        return pos;
    }
    let mut depth = 0i32;
    while pos < n {
        let c = bytes[pos];
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return pos + 1;
            }
        }
        pos += 1;
    }
    n
}

const CONTROL: &[&str] = &[
    "if",
    "else",
    "for",
    "while",
    "switch",
    "catch",
    "try",
    "finally",
    "return",
    "do",
    "new",
    "throw",
    "await",
    "yield",
    "typeof",
    "delete",
    "case",
    "default",
    "with",
    "elif",
    "match",
    "loop",
    "unsafe",
    "go",
    "defer",
    "select",
    "sizeof",
    "using",
    "lock",
    "synchronized",
    "fixed",
    "foreach",
    "when",
    "guard",
    "repeat",
    "assert",
    "in",
    "of",
    "constructor",
];

fn is_head_char(c: char) -> bool {
    c.is_ascii() && (is_ident_byte(c as u8) || " <>[],.:&*".contains(c))
}

/// The name of a method/function whose signature is a single line ending in `{`.
///
/// Covers declarations without a leading keyword (JS/TS class methods, Java, C#, C, C++, PHP). Deliberately strict:
/// the first parenthesis group must close on the line and be followed only by an optional return type, and
/// control-flow keywords, calls on objects and callbacks never qualify.
pub fn method_symbol(line: &str) -> Option<String> {
    let s = line.trim();
    let sb = s.as_bytes();
    if !s.ends_with('{') || !is_ident_byte(sb[0]) || sb[0].is_ascii_digit() {
        return None;
    }
    let p = s.find('(')?;
    if p == 0 {
        return None;
    }
    let end = skip_group(s, p, b'(', b')');
    if end >= s.len() {
        return None;
    }
    let body = s[end..s.len() - 1].trim();
    if !body.is_empty()
        && !["->", ":", "throws ", "const", "override", "final", "noexcept", "where "]
            .iter()
            .any(|x| body.starts_with(x))
    {
        return None;
    }
    let prefix = s[..p].trim_end();
    let pb = prefix.as_bytes();
    let mut j = pb.len();
    while j > 0 && is_ident_byte(pb[j - 1]) {
        j -= 1;
    }
    let name = &prefix[j..];
    if name.is_empty() || name.as_bytes()[0].is_ascii_digit() || (j > 0 && pb[j - 1] == b'.') {
        return None;
    }
    let head = &prefix[..j];
    if !head.chars().all(is_head_char) {
        return None;
    }
    let head_toks = ident_tokens(head);
    if head_toks.len() > 6 {
        return None;
    }
    let first = ident_tokens(s)[0].0;
    if CONTROL.contains(&first) || CONTROL.contains(&name) || DECL.contains(&name) {
        return None;
    }
    let indented = line.starts_with(' ') || line.starts_with('\t');
    if head_toks.is_empty() && !indented && !s[p + 1..end - 1].trim().is_empty() {
        return None; // unindented bare `name(args) {` is more likely a call than a declaration
    }
    Some(name.to_string())
}

/// The name declared by `line` (func/class/fn/struct/def/type/method/...), if any.
pub fn line_symbol(line: &str) -> Option<String> {
    let s = line.trim();
    if s.is_empty() || ["//", "/*", "*", "#", "--", ";"].iter().any(|p| s.starts_with(p)) {
        return None;
    }
    method_symbol(line).or_else(|| keyword_symbol(s))
}

fn keyword_symbol(s: &str) -> Option<String> {
    let toks = ident_tokens(s);
    for (idx, &(tok, _, b)) in toks.iter().enumerate() {
        if DECL.contains(&tok) {
            let nxt = toks.get(idx + 1).map(|t| t.0).unwrap_or("");
            if (tok == "const" || tok == "static") && DECL.contains(&nxt) {
                continue;
            }
            let mut pos = b;
            if tok == "func" {
                pos = skip_group(s, pos, b'(', b')');
            } else if tok == "impl" {
                pos = skip_group(s, pos, b'<', b'>');
            }
            let mut rest: Vec<&(&str, usize, usize)> = toks.iter().filter(|t| t.1 >= pos).collect();
            if tok == "impl" {
                if let Some(i) = rest.iter().position(|t| t.0 == "for") {
                    rest = rest[i + 1..].to_vec();
                }
            }
            while !rest.is_empty() && ["mut", "ref", "self", "dyn"].contains(&rest[0].0) {
                rest.remove(0);
            }
            if rest.is_empty() || DECL.contains(&rest[0].0) || MODIFIERS.contains(&rest[0].0) {
                return None;
            }
            return Some(rest[0].0.to_string());
        }
        if !MODIFIERS.contains(&tok) {
            return None;
        }
    }
    None
}

/// Declared names in a chunk plus their sub-words, space separated (FTS `symbols` column).
pub fn symbols_of(kind: &str, text: &str) -> String {
    let mut names: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    if kind == "doc" {
        for ln in text.split('\n') {
            let t = ln.trim_start();
            if t.starts_with('#') {
                for (tok, _, _) in ident_tokens(t) {
                    if seen.insert(tok.to_lowercase()) {
                        names.push(tok.to_string());
                    }
                }
            }
        }
    } else if kind == "code" {
        for ln in text.split('\n') {
            if let Some(name) = line_symbol(ln) {
                if seen.insert(name.to_lowercase()) {
                    names.push(name);
                }
            }
            if names.len() >= MAX_SYMBOLS {
                break;
            }
        }
    }
    if names.is_empty() {
        return String::new();
    }
    let joined = names.join(" ");
    let sub = terms_of(&joined);
    if sub.is_empty() {
        joined
    } else {
        format!("{} {}", joined, sub)
    }
}

const LOW_PRIORITY_DIRS: &[&str] = &[
    "test",
    "tests",
    "testdata",
    "spec",
    "specs",
    "example",
    "examples",
    "benches",
    "bench",
    "benchmarks",
    "fixtures",
    "fixture",
    "__tests__",
    "__mocks__",
    "mocks",
    "vendor",
    "third_party",
    "thirdparty",
    "node_modules",
    "e2e",
];
pub const LOW_PRIORITY_QUERY_WORDS: &[&str] = &[
    "test",
    "tests",
    "testing",
    "spec",
    "example",
    "examples",
    "sample",
    "fixture",
    "benchmark",
    "bench",
    "mock",
];

/// True for paths an agent rarely wants first: tests, examples, fixtures, vendored code.
pub fn is_low_priority(path: &str) -> bool {
    let lower = path.to_lowercase();
    let parts: Vec<&str> = lower.split('/').collect();
    if parts[..parts.len() - 1].iter().any(|p| LOW_PRIORITY_DIRS.contains(p)) {
        return true;
    }
    let name = parts[parts.len() - 1];
    let stem = match name.rfind('.') {
        Some(i) => &name[..i],
        None => name,
    };
    ["_test", ".test", "_spec", ".spec", "_tests"]
        .iter()
        .any(|e| stem.ends_with(e))
        || stem.starts_with("test_")
        || name == "conftest.py"
        || name.contains(".test.")
        || name.contains(".spec.")
        || stem.starts_with("example")
}
