//! Dynamic-dispatch boundaries: where a symbol's NAME appears in a shape the
//! static graph does not turn into an edge.
//!
//! When `callgraph` / `impact` / `refs` (and their MCP twins) find no caller for
//! a function, "nobody calls it" and "it is called through a string key, a
//! reflection primitive, an event name or a function value" look identical.
//! This module tells them apart by DISCLOSURE only: it never adds an edge, and
//! it runs only on an empty caller result, so a non-empty answer is unchanged.
//!
//! # Accepted shapes (one site per line; the first matching kind wins)
//!
//! | kind                 | example                                            |
//! |----------------------|----------------------------------------------------|
//! | `reflection`         | `getattr(o, "save")`, `send(:save)`, `getMethod("save")`, `dlsym(h, "save")` |
//! | `event_name`         | `bus.on("save", f)`, `emit('save')`, `ipcRenderer.invoke("save")` |
//! | `string_key`         | `handlers["save"]`, `{"save": f}`, `'save' => …`   |
//! | `symbol`             | Ruby `before_action :save`                         |
//! | `function_reference` | `register(save)`, `.map(Self::save)`, `{ save, load }`, `x = save` |
//!
//! Look-alikes deliberately NOT reported: comments; strings that merely
//! mention the name (`log("save")`, `"save failed"`); identifiers containing
//! it (`autosave`, `save_all`, `$save`); the definition; a direct call
//! `save(…)` (a static edge — or a resolver gap, which is not dispatch);
//! imports / re-exports / destructuring; parameters; conditions
//! (`if (x.save)`); ternaries and `case "save":`. Ruby has no
//! function-reference shape (a bare `save` there IS a call). The corpus in
//! `tests.rs` pins every row of this table.
//!
//! Comments and string contents are blanked with byte offsets preserved, so
//! a shape is matched against code only — except where the string IS the key
//! (`handlers["save"]`, `getattr(o, "save")`), which is read from the literal.
//!
//! Test files are not scanned (a spec's `receive(:save)` is not production
//! dispatch), and neither are languages without a shape table (markdown,
//! json, html, css, bash). An answer that left something unread says so
//! (`not scanned: …`) instead of printing the complete "none" line: a
//! definition in a language without a table, a file over 2 MB or not UTF-8,
//! files the query did not reach within [`SCAN_TIME_LIMIT`], a Rust value
//! `Q::name` whose `Q` is not known to be where the name is defined.
//!
//! Rust has no member-value shape: `a.b` not followed by `(` is a field read
//! there (a method is passed as `Self::b` / `Type::b`).

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use rusqlite::Connection;

/// Sites listed per answer; the rest are counted.
pub const BOUNDARY_SITE_CAP: usize = 5;

/// Files larger than this are skipped (minified bundles, generated tables).
const MAX_SCAN_BYTES: u64 = 2 * 1024 * 1024;

/// Wall-clock cap on one query's scan. The scan is linear in the bytes read,
/// so this fires only on a very large repository or a pathological file; the
/// files not reached are counted in the answer.
pub const SCAN_TIME_LIMIT: Duration = Duration::from_millis(1000);

/// Kind of dynamic-dispatch shape. Declaration order is priority order: when
/// one line carries several shapes, the smallest wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Shape {
    Reflection,
    EventName,
    StringKey,
    Symbol,
    FunctionReference,
}

impl Shape {
    /// Machine name (JSON `shape`).
    pub fn as_str(self) -> &'static str {
        match self {
            Shape::Reflection => "reflection",
            Shape::EventName => "event_name",
            Shape::StringKey => "string_key",
            Shape::Symbol => "symbol",
            Shape::FunctionReference => "function_reference",
        }
    }

    /// Human label (text output).
    pub fn label(self) -> &'static str {
        match self {
            Shape::Reflection => "reflection",
            Shape::EventName => "event name",
            Shape::StringKey => "string key",
            Shape::Symbol => "symbol",
            Shape::FunctionReference => "function reference",
        }
    }
}

/// One shape found in one source text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShapeHit {
    /// 1-based line.
    pub line: usize,
    pub shape: Shape,
    /// The dispatching callee for `reflection` / `event_name` (`getattr`, `on`).
    pub via: Option<String>,
}

/// Callees that look a member up by name.
const REFLECTION_CALLEES: &[&str] = &[
    "getattr",
    "hasattr",
    "methodcaller",
    "send",
    "public_send",
    "__send__",
    "method",
    "instance_method",
    "public_method",
    "respond_to?",
    "getMethod",
    "getDeclaredMethod",
    "MethodByName",
    "GetMethod",
    "InvokeMember",
    "invokeMethod",
    "dlsym",
    "GetProcAddress",
    "call_user_func",
    "call_user_func_array",
    "method_exists",
    "is_callable",
];

/// Callees that route by an event / channel / command name.
const EVENT_CALLEES: &[&str] = &[
    "on",
    "once",
    "off",
    "emit",
    "addListener",
    "prependListener",
    "removeListener",
    "addEventListener",
    "removeEventListener",
    "subscribe",
    "publish",
    "dispatch",
    "trigger",
    "handle",
    "invoke",
    "$on",
    "$emit",
];

/// A call whose parenthesized argument is a condition, not a value.
const CONTROL_KEYWORDS: &[&str] = &[
    "if", "while", "for", "switch", "match", "elif", "catch", "until", "unless",
];

/// A call-looking `(` that opens a parameter list.
const DEF_KEYWORDS: &[&str] = &["def", "fn", "function", "func", "fun"];

#[derive(Clone, Copy)]
enum SingleQuote {
    /// `'…'` is a string (JS, Python, Ruby, PHP, Dart).
    Str,
    /// `'x'` is a char literal (C, Java, Go, C#, Kotlin, Swift).
    Char,
    /// `'x'` is a char, `'a` a lifetime (Rust).
    RustChar,
}

#[derive(Clone, Copy)]
struct Syntax {
    slash_comments: bool,
    hash_comments: bool,
    nested_block_comments: bool,
    single_quote: SingleQuote,
    /// Backtick string (JS template, Go raw string).
    backtick: bool,
    /// `${…}` interpolation inside backtick strings (JS/TS).
    template_interp: bool,
    triple_quotes: bool,
    rust_raw: bool,
    /// `"…"` may span lines (Rust, Ruby, PHP).
    multiline_dquote: bool,
    ruby: bool,
    /// `$` is an identifier character (JS/TS, Dart, PHP variables).
    dollar_ident: bool,
    /// `"save": …` is a table key. Not Rust, where it only occurs as `json!`
    /// data.
    key_colon: bool,
    /// `"save" => …` is a table key (Ruby / PHP hash rocket; in Rust and Scala
    /// `=>` is a match arm).
    key_rocket: bool,
    /// `x["save"] = …` stores data, never a handler (Rust: `HashMap` has no
    /// `IndexMut`, so a subscript assignment is always `serde_json`).
    index_store_is_data: bool,
    /// `if (…) {` — conditions are parenthesized, so `name(…) {` opens a
    /// method body (JS/TS, Java, C/C++, C#, Kotlin, Dart, PHP).
    paren_conditions: bool,
    /// `save.bind(…)` / `.call(…)` / `.apply(…)` hands the function on (JS/TS).
    function_methods: bool,
    /// `obj.save` (not called) can be the method as a value. Not in Rust,
    /// where it is always a field read.
    member_value: bool,
    /// `Q::save` names a function only when `Q` is where one of the
    /// definitions lives (Rust: `thread::JoinHandle::join` is std's, not a
    /// `join` of this project's). Any other `Q` is not a site, but it is
    /// counted as undecided: the crate's own name, an inline `mod`, a
    /// `use … as` alias or a trait can be where the name lives.
    check_path_qualifier: bool,
}

fn syntax_for(language: &str) -> Option<Syntax> {
    let base = Syntax {
        slash_comments: true,
        hash_comments: false,
        nested_block_comments: false,
        single_quote: SingleQuote::Char,
        backtick: false,
        template_interp: false,
        triple_quotes: false,
        rust_raw: false,
        multiline_dquote: false,
        ruby: false,
        dollar_ident: false,
        key_colon: true,
        key_rocket: false,
        index_store_is_data: false,
        paren_conditions: true,
        function_methods: false,
        member_value: true,
        check_path_qualifier: false,
    };
    Some(match language {
        "javascript" | "typescript" | "tsx" => Syntax {
            single_quote: SingleQuote::Str,
            backtick: true,
            template_interp: true,
            dollar_ident: true,
            function_methods: true,
            ..base
        },
        "go" => Syntax {
            backtick: true,
            paren_conditions: false,
            ..base
        },
        "rust" => Syntax {
            nested_block_comments: true,
            single_quote: SingleQuote::RustChar,
            rust_raw: true,
            multiline_dquote: true,
            key_colon: false,
            index_store_is_data: true,
            paren_conditions: false,
            member_value: false,
            check_path_qualifier: true,
            ..base
        },
        "java" | "c" | "cpp" | "csharp" => base,
        "kotlin" => Syntax {
            nested_block_comments: true,
            triple_quotes: true,
            ..base
        },
        "swift" => Syntax {
            nested_block_comments: true,
            triple_quotes: true,
            paren_conditions: false,
            ..base
        },
        "dart" => Syntax {
            nested_block_comments: true,
            single_quote: SingleQuote::Str,
            triple_quotes: true,
            dollar_ident: true,
            ..base
        },
        "php" => Syntax {
            hash_comments: true,
            single_quote: SingleQuote::Str,
            multiline_dquote: true,
            dollar_ident: true,
            key_rocket: true,
            ..base
        },
        "python" => Syntax {
            slash_comments: false,
            hash_comments: true,
            single_quote: SingleQuote::Str,
            triple_quotes: true,
            paren_conditions: false,
            ..base
        },
        "ruby" => Syntax {
            slash_comments: false,
            hash_comments: true,
            single_quote: SingleQuote::Str,
            multiline_dquote: true,
            ruby: true,
            key_rocket: true,
            paren_conditions: false,
            ..base
        },
        _ => return None,
    })
}

/// A string literal: quote positions in the source and its content, when the
/// content is static (no interpolation).
struct StrLit {
    open: usize,
    close: usize,
    content: Option<String>,
}

/// Source with comments and string contents blanked to spaces (newlines kept,
/// offsets preserved), plus the string literals found.
struct Lexed {
    masked: Vec<u8>,
    strings: Vec<StrLit>,
}

fn is_ident_byte(b: u8, syn: &Syntax) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80 || (syn.dollar_ident && b == b'$')
}

fn blank(masked: &mut [u8], from: usize, to: usize) {
    for b in &mut masked[from..to] {
        if *b != b'\n' {
            *b = b' ';
        }
    }
}

/// Byte length of the UTF-8 char starting at `i`.
fn char_len(src: &[u8], i: usize) -> usize {
    match src.get(i) {
        Some(b) if *b < 0x80 => 1,
        Some(b) if *b >= 0xF0 => 4,
        Some(b) if *b >= 0xE0 => 3,
        Some(_) => 2,
        None => 0,
    }
}

fn lex(src: &[u8], syn: &Syntax) -> Lexed {
    let mut masked = src.to_vec();
    let mut strings = Vec::new();
    let n = src.len();
    let mut i = 0;
    // Brace depth of each open `${` (JS template interpolation).
    let mut interp: Vec<usize> = Vec::new();
    let at_line_start = |i: usize| i == 0 || src[i - 1] == b'\n';
    while i < n {
        let c = src[i];
        // Resume a template literal when its `${…}` closes.
        if !interp.is_empty() {
            if c == b'{' {
                *interp.last_mut().unwrap() += 1;
            } else if c == b'}' {
                let top = interp.last_mut().unwrap();
                if *top == 0 {
                    interp.pop();
                    i = scan_template(src, &mut masked, i + 1, &mut interp, &mut strings, None);
                    continue;
                }
                *top -= 1;
            }
        }
        // Comments.
        if syn.slash_comments && c == b'/' && i + 1 < n {
            if src[i + 1] == b'/' {
                let end = src[i..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(n, |p| i + p);
                blank(&mut masked, i, end);
                i = end;
                continue;
            }
            if src[i + 1] == b'*' {
                let mut depth = 1usize;
                let mut j = i + 2;
                while j < n && depth > 0 {
                    if syn.nested_block_comments && src[j] == b'/' && src.get(j + 1) == Some(&b'*')
                    {
                        depth += 1;
                        j += 2;
                    } else if src[j] == b'*' && src.get(j + 1) == Some(&b'/') {
                        depth -= 1;
                        j += 2;
                    } else {
                        j += 1;
                    }
                }
                blank(&mut masked, i, j);
                i = j;
                continue;
            }
        }
        if syn.hash_comments && c == b'#' {
            let end = src[i..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(n, |p| i + p);
            blank(&mut masked, i, end);
            i = end;
            continue;
        }
        if syn.ruby && c == b'=' && at_line_start(i) && src[i..].starts_with(b"=begin") {
            let end = find_sub(src, i, b"\n=end").map_or(n, |p| {
                src[p + 1..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(n, |q| p + 1 + q)
            });
            blank(&mut masked, i, end);
            i = end;
            continue;
        }
        // Rust raw strings: r"…", r#"…"#, br"…".
        if syn.rust_raw
            && c == b'r'
            && (i == 0 || !is_ident_byte(src[i - 1], syn) || src[i - 1] == b'b')
        {
            let mut j = i + 1;
            while j < n && src[j] == b'#' {
                j += 1;
            }
            let hashes = j - i - 1;
            if j < n && src[j] == b'"' {
                let mut term = vec![b'"'];
                term.extend(std::iter::repeat_n(b'#', hashes));
                let close = find_sub(src, j + 1, &term).unwrap_or(n);
                record(src, &mut masked, &mut strings, j, close, true);
                i = (close + term.len()).min(n);
                continue;
            }
        }
        // Triple-quoted strings.
        if syn.triple_quotes && (c == b'"' || c == b'\'') && src[i..].starts_with(&[c, c, c]) {
            let close = find_sub(src, i + 3, &[c, c, c]).unwrap_or(n);
            blank(&mut masked, i + 3, close);
            strings.push(StrLit {
                open: i,
                close,
                content: None,
            });
            i = (close + 3).min(n);
            continue;
        }
        if c == b'"' {
            let close = scan_quoted(src, i + 1, b'"', syn.multiline_dquote);
            record(src, &mut masked, &mut strings, i, close, false);
            i = (close + 1).min(n);
            continue;
        }
        if c == b'\'' {
            match syn.single_quote {
                SingleQuote::Str => {
                    let close = scan_quoted(src, i + 1, b'\'', syn.multiline_dquote);
                    record(src, &mut masked, &mut strings, i, close, false);
                    i = (close + 1).min(n);
                    continue;
                }
                SingleQuote::Char => {
                    let close = scan_quoted(src, i + 1, b'\'', false);
                    blank(&mut masked, i + 1, close);
                    i = (close + 1).min(n);
                    continue;
                }
                SingleQuote::RustChar => {
                    // `'\…'` or `'x'` is a char; anything else (`'a`, `'outer:`)
                    // is a lifetime or label and stays code.
                    let is_char = match src.get(i + 1) {
                        Some(b'\\') => true,
                        Some(_) => src.get(i + 1 + char_len(src, i + 1)) == Some(&b'\''),
                        None => false,
                    };
                    if is_char {
                        let close = scan_quoted(src, i + 1, b'\'', false);
                        blank(&mut masked, i + 1, close);
                        i = (close + 1).min(n);
                        continue;
                    }
                }
            }
        }
        if syn.backtick && c == b'`' {
            if syn.template_interp {
                i = scan_template(src, &mut masked, i + 1, &mut interp, &mut strings, Some(i));
            } else {
                let close = src[i + 1..]
                    .iter()
                    .position(|&b| b == b'`')
                    .map_or(n, |p| i + 1 + p);
                record(src, &mut masked, &mut strings, i, close, true);
                i = (close + 1).min(n);
            }
            continue;
        }
        i += 1;
    }
    Lexed { masked, strings }
}

/// Scan a JS template body from `from`; returns the index after the closing
/// backtick, or after a `${` (pushing an interpolation frame). `open` is the
/// opening backtick when this is the literal's first segment — a literal with
/// an interpolation has no static content.
fn scan_template(
    src: &[u8],
    masked: &mut [u8],
    from: usize,
    interp: &mut Vec<usize>,
    strings: &mut Vec<StrLit>,
    open: Option<usize>,
) -> usize {
    let n = src.len();
    let mut j = from;
    while j < n {
        match src[j] {
            b'\\' => j += 2,
            b'`' => {
                blank(masked, from, j);
                if let Some(o) = open {
                    strings.push(StrLit {
                        open: o,
                        close: j,
                        content: std::str::from_utf8(&src[from..j]).ok().map(str::to_string),
                    });
                }
                return j + 1;
            }
            b'$' if src.get(j + 1) == Some(&b'{') => {
                blank(masked, from, j);
                interp.push(0);
                return j + 2;
            }
            _ => j += 1,
        }
    }
    blank(masked, from, n);
    n
}

/// Index of the closing quote (or of the newline / end that stops an
/// unterminated single-line literal).
fn scan_quoted(src: &[u8], from: usize, quote: u8, multiline: bool) -> usize {
    let n = src.len();
    let mut j = from;
    while j < n {
        let b = src[j];
        if b == b'\\' {
            j += 2;
            continue;
        }
        if b == quote || (!multiline && b == b'\n') {
            return j;
        }
        j += 1;
    }
    n
}

fn record(
    src: &[u8],
    masked: &mut [u8],
    strings: &mut Vec<StrLit>,
    open: usize,
    close: usize,
    raw: bool,
) {
    let close = close.min(src.len());
    let body = &src[open + 1..close];
    blank(masked, open + 1, close);
    let content = if !raw && body.contains(&b'\\') {
        None
    } else {
        std::str::from_utf8(body).ok().map(str::to_string)
    };
    strings.push(StrLit {
        open,
        close,
        content,
    });
}

fn find_sub(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from > hay.len() || needle.is_empty() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| from + p)
}

/// Previous non-blank byte strictly before `i`: (index, byte).
fn prev_non_ws(m: &[u8], i: usize) -> Option<(usize, u8)> {
    let mut j = i;
    while j > 0 {
        j -= 1;
        if !m[j].is_ascii_whitespace() {
            return Some((j, m[j]));
        }
    }
    None
}

/// Next non-blank byte at or after `i`: (index, byte, crossed a newline).
fn next_non_ws(m: &[u8], i: usize) -> Option<(usize, u8, bool)> {
    let mut nl = false;
    for (j, &b) in m.iter().enumerate().skip(i) {
        if b == b'\n' {
            nl = true;
        } else if !b.is_ascii_whitespace() {
            return Some((j, b, nl));
        }
    }
    None
}

/// Identifier ending at `end` (exclusive) — Ruby's `?`/`!` suffix included.
fn ident_before(m: &[u8], end: usize, syn: &Syntax) -> (usize, String) {
    let mut s = end;
    if syn.ruby && s > 0 && matches!(m[s - 1], b'?' | b'!') {
        s -= 1;
    }
    while s > 0 && is_ident_byte(m[s - 1], syn) {
        s -= 1;
    }
    (s, String::from_utf8_lossy(&m[s..end]).into_owned())
}

/// Masked source plus its bracket pairs and line starts, computed once per
/// file. Every per-occurrence question ("which bracket encloses this?",
/// "where does it close?", "which line is this?") is then a binary search.
/// Answering them by scanning made a deeply nested file quadratic: one forward
/// scan to the matching close per occurrence took 21 s on 240 KB.
struct Src<'a> {
    m: &'a [u8],
    /// Position of every bracket byte, ascending.
    brackets: Vec<usize>,
    /// Per bracket: an opener's matching close, if any.
    close: Vec<Option<usize>>,
    /// Per bracket: the innermost opener still open just after it.
    open_after: Vec<Option<usize>>,
    /// Byte offset of each line's first byte.
    line_starts: Vec<usize>,
}

impl<'a> Src<'a> {
    fn new(m: &'a [u8]) -> Self {
        let mut brackets = Vec::new();
        let mut close: Vec<Option<usize>> = Vec::new();
        let mut open_after = Vec::new();
        // Indices into `brackets` of the openers not yet closed. Any closer
        // closes the innermost opener, whatever its kind — the same pairing
        // as counting depth over all three kinds, which is what the scans
        // this replaces did.
        let mut stack: Vec<usize> = Vec::new();
        for (j, &b) in m.iter().enumerate() {
            match b {
                b'(' | b'[' | b'{' => {
                    stack.push(brackets.len());
                    brackets.push(j);
                    close.push(None);
                    open_after.push(Some(j));
                }
                b')' | b']' | b'}' => {
                    if let Some(k) = stack.pop() {
                        close[k] = Some(j);
                    }
                    brackets.push(j);
                    close.push(None);
                    open_after.push(stack.last().map(|&k| brackets[k]));
                }
                _ => {}
            }
        }
        let line_starts = std::iter::once(0)
            .chain(
                m.iter()
                    .enumerate()
                    .filter(|(_, &b)| b == b'\n')
                    .map(|(i, _)| i + 1),
            )
            .collect();
        Src {
            m,
            brackets,
            close,
            open_after,
            line_starts,
        }
    }

    /// The unmatched opener enclosing `pos`, at most 4 KB back.
    fn enclosing_opener(&self, pos: usize) -> Option<usize> {
        let k = self.brackets.partition_point(|&p| p < pos);
        let o = self.open_after[k.checked_sub(1)?]?;
        (o >= pos.saturating_sub(4096)).then_some(o)
    }

    /// The close matching the opener at `open`.
    fn matching_close(&self, open: usize) -> Option<usize> {
        let k = self.brackets.binary_search(&open).ok()?;
        self.close[k]
    }

    /// 1-based line of `pos`.
    fn line_no(&self, pos: usize) -> usize {
        self.line_starts.partition_point(|&s| s <= pos)
    }

    /// Text of the line containing `pos`, trimmed.
    fn line_of(&self, pos: usize) -> &'a [u8] {
        let l = self.line_no(pos);
        let s = self.line_starts[l - 1];
        let e = self.line_starts.get(l).map_or(self.m.len(), |&n| n - 1);
        self.m[s..e].trim_ascii()
    }
}

/// Name of the call whose argument list encloses `pos`: the nearest `(`,
/// looking through at most one array/object literal (`f([$this, 'save'])`).
fn enclosing_callee(src: &Src, pos: usize, syn: &Syntax) -> Option<String> {
    let m = src.m;
    let mut at = pos;
    for _ in 0..2 {
        let o = src.enclosing_opener(at)?;
        if m[o] == b'(' {
            let (_, name) =
                prev_non_ws(m, o).map_or((0, String::new()), |(k, _)| ident_before(m, k + 1, syn));
            return (!name.is_empty()).then_some(name);
        }
        at = o;
    }
    None
}

/// The word naming a parenthesized list at `o` — skipping a generic parameter
/// list between them (`fn map<T, F>(`, `fn arg<S: AsRef<OsStr>>(`): the start
/// of that word. `None` when no identifier precedes.
fn list_owner(m: &[u8], o: usize, syn: &Syntax) -> Option<usize> {
    let (mut k, mut b) = prev_non_ws(m, o)?;
    if b == b'>' && !(k > 0 && m[k - 1] == b'-') {
        // Back over one balanced `<…>`, ignoring the `>` of a `->`.
        let floor = k.saturating_sub(512);
        let mut depth = 0usize;
        let mut j = k + 1;
        loop {
            if j <= floor {
                return None;
            }
            j -= 1;
            match m[j] {
                b'>' if !(j > 0 && m[j - 1] == b'-') => depth += 1,
                b'<' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
        (k, b) = prev_non_ws(m, j)?;
    }
    if !is_ident_byte(b, syn) {
        return None;
    }
    let (ws, _) = ident_before(m, k + 1, syn);
    Some(ws)
}

/// Is the list opened at `o` a definition's parameter list — `def name(`,
/// `fn name(`, `fn name<T>(`, `function name(`?
fn opens_def_params(m: &[u8], o: usize, syn: &Syntax) -> bool {
    list_owner(m, o, syn)
        .and_then(|ws| word_before(m, ws, syn))
        .is_some_and(|kw| DEF_KEYWORDS.contains(&kw.as_str()))
}

/// Ruby paren-less call: `send :save` / `send "save"` — the identifier just
/// before `pos` on the same line, separated by spaces only.
fn parenless_callee(m: &[u8], pos: usize, syn: &Syntax) -> Option<String> {
    let mut j = pos;
    let mut spaces = 0;
    while j > 0 && (m[j - 1] == b' ' || m[j - 1] == b'\t') {
        j -= 1;
        spaces += 1;
    }
    if spaces == 0 {
        return None;
    }
    let (_, name) = ident_before(m, j, syn);
    (!name.is_empty()).then_some(name)
}

fn classify_callee(name: &str) -> Option<Shape> {
    if REFLECTION_CALLEES.contains(&name) {
        Some(Shape::Reflection)
    } else if EVENT_CALLEES.contains(&name) {
        Some(Shape::EventName)
    } else {
        None
    }
}

fn is_import_line(line: &[u8]) -> bool {
    const HEADS: &[&str] = &[
        "import ",
        "import{",
        "from ",
        "use ",
        "pub use ",
        "pub(crate) use ",
        "pub(super) use ",
        "#include",
        "using ",
        "extern crate",
        "package ",
        "export {",
        "export{",
        "export *",
        "export type {",
        "export default",
    ];
    HEADS.iter().any(|h| line.starts_with(h.as_bytes()))
}

fn word_before(m: &[u8], pos: usize, syn: &Syntax) -> Option<String> {
    let (k, _) = prev_non_ws(m, pos)?;
    let (_, w) = ident_before(m, k + 1, syn);
    (!w.is_empty()).then_some(w)
}

/// Is the identifier at `[s, e)` written without a qualifier (`save`, not
/// `obj.save` / `Store::save` / `$this->save`)?
fn is_bare(m: &[u8], s: usize) -> bool {
    match prev_non_ws(m, s) {
        Some((p, b'.')) => p > 0 && m[p - 1] == b'.',
        Some((p, b':')) => !(p > 0 && m[p - 1] == b':'),
        Some((p, b'>')) => !(p > 0 && m[p - 1] == b'-'),
        _ => true,
    }
}

/// Does the identifier at `[s, e)` DECLARE a local of that name — a
/// variable, a parameter, a loop variable, a pattern binding? A file that
/// binds the name locally uses the bare name for the local, so its bare
/// references are not the function's (`const url = …; fetch(url)`).
fn is_binding(src: &Src, s: usize, e: usize, syn: &Syntax) -> bool {
    let m = src.m;
    if !is_bare(m, s) {
        return false;
    }
    // `run = save` binds `run`; `save` is the value.
    if let Some((p, b'=')) = prev_non_ws(m, s) {
        if !(p > 0 && matches!(m[p - 1], b'=' | b'!' | b'<' | b'>')) {
            return false;
        }
    }
    if let Some(w) = word_before(m, s, syn) {
        if matches!(
            w.as_str(),
            "const" | "let" | "var" | "val" | "mut" | "for" | "as" | "lambda"
        ) {
            return true;
        }
    }
    let rest = m[e..].trim_ascii_start();
    // Go `url := …`.
    if rest.starts_with(b":=") {
        return true;
    }
    // Statement-start assignment `url = …` (Python, Ruby, Go, JS re-binding).
    if rest.starts_with(b"=") && !rest.starts_with(b"==") && !rest.starts_with(b"=>") {
        match prev_non_ws(m, s) {
            None => return true,
            Some((p, b)) => {
                let newline_between = m[p + 1..s].contains(&b'\n');
                if newline_between || matches!(b, b';' | b'{' | b'}') {
                    return true;
                }
            }
        }
    }
    // Closure parameters `|url|`, `|url, b|`, `|url: T|`, `|a, url|` — not a
    // bitwise `a | url`.
    let prev = prev_non_ws(m, s);
    let single_bar = matches!(prev, Some((p, b'|')) if p == 0 || m[p - 1] != b'|');
    if (single_bar && (rest.starts_with(b"|") || rest.starts_with(b",") || rest.starts_with(b":")))
        || (matches!(prev, Some((_, b','))) && rest.starts_with(b"|"))
    {
        return true;
    }
    // Inside a parameter list or a destructuring pattern.
    let Some(o) = src.enclosing_opener(s) else {
        return false;
    };
    let after_close = src.matching_close(o).map(|c| m[c + 1..].trim_ascii_start());
    if let Some(r) = after_close {
        if r.starts_with(b"=>") || (r.starts_with(b"=") && !r.starts_with(b"==")) {
            return true;
        }
    }
    match m[o] {
        b'(' => {
            let callee = word_before(m, o, syn);
            if callee
                .as_deref()
                .is_some_and(|c| CONTROL_KEYWORDS.contains(&c))
            {
                return false;
            }
            if callee.as_deref().is_some_and(|c| DEF_KEYWORDS.contains(&c)) {
                return true;
            }
            if opens_def_params(m, o, syn) {
                return true;
            }
            // `m(url) {` / `void f(String url) {` — a method's parameters, in
            // languages whose conditions are parenthesized (in Rust / Go /
            // Swift `if check(url) {` is a call inside a condition).
            syn.paren_conditions && after_close.is_some_and(|r| r.starts_with(b"{"))
        }
        b'{' => {
            word_before(m, o, syn).is_some_and(|w| matches!(w.as_str(), "const" | "let" | "var"))
        }
        _ => false,
    }
}

/// Is the identifier at `[s, e)` a function used as a value? `qualifiers`,
/// when known, are the path segments that can precede the name and still
/// mean one of its definitions (see [`Syntax::check_path_qualifier`]).
/// `unresolved` is set when the answer is yes except that the path's own
/// qualifier is not among them, or is a generic or qualified-self path
/// (`Wrapper::<u8>::save`, `<Db as Store>::save`): the crate's own name, an
/// inline `mod`, a `use … as` alias and a trait all look like that, so such a
/// value is neither a site nor proof that none exists.
fn is_function_reference(
    src: &Src,
    s: usize,
    e: usize,
    syn: &Syntax,
    qualifiers: Option<&[String]>,
    unresolved: &mut bool,
) -> bool {
    let m = src.m;
    // `logerror.bind(this)`, `handler.call(ctx)`: the function object itself
    // is used, whatever surrounds it.
    if syn.function_methods
        && [&b".bind("[..], b".call(", b".apply("]
            .iter()
            .any(|t| m[e..].starts_with(t))
    {
        return !is_import_line(src.line_of(s));
    }
    // What follows: a value ends at a separator; `(` is a call, `=` an
    // assignment target, `:` a key / annotation, `.` a property access.
    let value_ends = match next_non_ws(m, e) {
        None => true,
        Some((_, b, false)) => matches!(b, b',' | b')' | b']' | b'}' | b';'),
        Some((_, b, true)) => is_ident_byte(b, syn) || matches!(b, b')' | b']' | b'}'),
    };
    if !value_ends {
        return false;
    }
    // Walk back over a qualifier chain: `a.b.save`, `Self::save`, `$this->save`, `::save`.
    let mut q = s;
    // Only the name's own qualifier (`Q` of `a::Q::save`) is checked.
    let mut own_qualifier = true;
    while let Some((p, b)) = prev_non_ws(m, q) {
        let sep_start = match b {
            // Rust `a.save` is a field read, never the method as a value.
            b'.' if !syn.member_value && (p == 0 || m[p - 1] != b'.') => return false,
            b'.' if p == 0 || m[p - 1] != b'.' => p,
            b':' if p > 0 && m[p - 1] == b':' => p - 1,
            b'>' if p > 0 && m[p - 1] == b'-' => p - 1,
            _ => break,
        };
        let qual = Syntax {
            dollar_ident: true,
            ..*syn
        };
        let (start, word) = ident_before(m, sep_start, &qual);
        if own_qualifier && b == b':' && syn.check_path_qualifier && !word.is_empty() {
            if let Some(known) = qualifiers {
                if !known.contains(&word) {
                    *unresolved = true;
                }
            }
        }
        if word.is_empty() {
            if own_qualifier
                && b == b':'
                && syn.check_path_qualifier
                && qualifiers.is_some()
                && matches!(prev_non_ws(m, sep_start), Some((_, b'>')))
            {
                // Rust `Wrapper::<u8>::save`, `<Db as Store>::save`.
                *unresolved = true;
                let line = src.line_of(s);
                return !(is_import_line(line)
                    || line.starts_with(b"#[")
                    || line.starts_with(b"#!["));
            }
            if b == b':' {
                // Kotlin/C++ `::save` — the chain starts at the separator.
                q = sep_start;
                break;
            }
            return false;
        }
        q = start;
        own_qualifier = false;
    }
    let Some((p, pb)) = prev_non_ws(m, q) else {
        return false;
    };
    let ok = match pb {
        b'(' | b'[' | b'{' | b',' => true,
        b'=' => !(p > 0 && matches!(m[p - 1], b'=' | b'!' | b'<' | b'>')),
        b':' => !(p > 0 && m[p - 1] == b':'),
        b'&' => !(p > 0 && m[p - 1] == b'&'),
        _ => word_before(m, q, syn).as_deref() == Some("return"),
    };
    if !ok {
        return false;
    }
    // An attribute's arguments are not values: `#[inline(never)]`, `#![allow(x)]`.
    let line = src.line_of(s);
    if is_import_line(line) || line.starts_with(b"#[") || line.starts_with(b"#![") {
        return false;
    }
    // The right-hand side of `=` is a value wherever it sits — including a
    // default parameter value (`function f(run = save)`, `{ install = save }`),
    // which the parameter-list and pattern rules below would otherwise reject.
    if pb == b'=' {
        return true;
    }
    // Enclosing brackets: parameter lists, conditions, imports, destructuring.
    let mut at = q;
    for _ in 0..3 {
        let Some(o) = src.enclosing_opener(at) else {
            break;
        };
        if is_import_line(src.line_of(o)) {
            return false;
        }
        // A bracket followed by `=` or `=>` is a pattern or a parameter list:
        // `let Some(save) = x`, `[a, save] = f()`, `(a, save) => a`,
        // `Some(save) => …`.
        if let Some(close) = src.matching_close(o) {
            let rest = m[close + 1..].trim_ascii_start();
            if rest.starts_with(b"=>") || (rest.starts_with(b"=") && !rest.starts_with(b"==")) {
                return false;
            }
        }
        match m[o] {
            b'(' => {
                let callee = word_before(m, o, syn);
                if let Some(c) = callee.as_deref() {
                    if CONTROL_KEYWORDS.contains(&c) || DEF_KEYWORDS.contains(&c) {
                        return false;
                    }
                }
                // `def name(`, `fn name(`, `fn name<T>(`, `function name(`.
                if opens_def_params(m, o, syn) {
                    return false;
                }
                // Stop at the innermost call: outer brackets belong to other expressions.
                break;
            }
            b'{' => {
                // `${save}` renders the value into text; nothing dispatches it.
                if o > 0 && m[o - 1] == b'$' {
                    return false;
                }
                if let Some(w) = word_before(m, o, syn) {
                    if matches!(w.as_str(), "const" | "let" | "var") {
                        return false;
                    }
                }
            }
            _ => {}
        }
        at = o;
    }
    true
}

/// Does a string literal `content` name `name` — exactly, or as the last
/// segment of a dotted / `::` path (`"app.tasks.save"`)?
fn names(content: &str, name: &str) -> bool {
    if content == name {
        return true;
    }
    content.strip_suffix(name).is_some_and(|head| {
        (head.ends_with('.') || head.ends_with("::") || head.ends_with('#'))
            && head
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | ':' | '#' | '$'))
    })
}

fn classify_string(
    src: &Src,
    lit: &StrLit,
    name: &str,
    syn: &Syntax,
) -> Option<(Shape, Option<String>)> {
    let content = lit.content.as_deref()?;
    if !names(content, name) {
        return None;
    }
    let m = src.m;
    let callee = enclosing_callee(src, lit.open, syn).or_else(|| {
        syn.ruby
            .then(|| parenless_callee(m, lit.open, syn))
            .flatten()
    });
    if let Some(c) = callee {
        if let Some(shape) = classify_callee(&c) {
            return Some((shape, Some(c)));
        }
    }
    if content != name {
        return None;
    }
    let before = prev_non_ws(m, lit.open);
    let after = next_non_ws(m, lit.close + 1).map(|(j, b, _)| (j, b));
    // `x["save"]` — except a Rust assignment target: `HashMap` has no
    // `IndexMut`, so `out["save"] = …` there is always `serde_json` data.
    if let (Some((_, b'[')), Some((j, b']'))) = (before, after) {
        let rest = m[j + 1..].trim_ascii_start();
        let data_store =
            syn.index_store_is_data && rest.starts_with(b"=") && !rest.starts_with(b"==");
        if !data_store {
            return Some((Shape::StringKey, None));
        }
    }
    // `{"save": …}` / `"save" => …`, but not `c ? "save" : x` nor `case "save":`.
    let key_follows = match after {
        Some((j, b':')) => syn.key_colon && m.get(j + 1) != Some(&b':'),
        Some((j, b'=')) => syn.key_rocket && m.get(j + 1) == Some(&b'>'),
        _ => false,
    };
    if key_follows
        && !matches!(before, Some((_, b'?')))
        && word_before(m, lit.open, syn).as_deref() != Some("case")
    {
        return Some((Shape::StringKey, None));
    }
    None
}

/// Every dynamic-dispatch site naming `name` in one source text, one per
/// line (most specific shape), in line order. Empty for a language without a
/// shape table, or a name that is not an identifier.
pub fn scan_source(language: &str, source: &str, name: &str) -> Vec<ShapeHit> {
    scan_source_with_defs(language, source, name, &[])
}

/// [`scan_source`], told which 1-based lines hold a definition of `name`.
pub fn scan_source_with_defs(
    language: &str,
    source: &str,
    name: &str,
    def_lines: &[usize],
) -> Vec<ShapeHit> {
    scan_source_until(language, source, name, def_lines, None, None)
        .map(|s| s.hits)
        .unwrap_or_default()
}

/// What one source text holds: its sites, and the lines of values that name
/// the function through a qualifier not known to be its own (see
/// [`is_function_reference`]) — undecided, so neither sites nor absent.
#[derive(Debug, Default, PartialEq, Eq)]
struct SourceScan {
    hits: Vec<ShapeHit>,
    unresolved_lines: Vec<usize>,
    /// Lines holding a call of the name ([`is_call_site`]), in a language
    /// [`call_family`] reads; empty in any other.
    call_lines: Vec<usize>,
}

/// [`scan_source_with_defs`] that gives up at `deadline` (`None` when it
/// did), told which path qualifiers name a definition (`None`: any).
fn scan_source_until(
    language: &str,
    source: &str,
    name: &str,
    def_lines: &[usize],
    qualifiers: Option<&[String]>,
    deadline: Option<Instant>,
) -> Option<SourceScan> {
    let Some(syn) = syntax_for(language) else {
        return Some(SourceScan::default());
    };
    if name.is_empty() || !source.contains(name) {
        return Some(SourceScan::default());
    }
    let Lexed { masked: m, strings } = lex(source.as_bytes(), &syn);
    let src = Src::new(&m);
    let past = || deadline.is_some_and(|d| Instant::now() >= d);

    let mut hits: std::collections::BTreeMap<usize, (Shape, Option<String>)> =
        std::collections::BTreeMap::new();
    let mut add = |pos: usize, shape: Shape, via: Option<String>| {
        let line = src.line_no(pos);
        match hits.get(&line) {
            Some((s, _)) if *s <= shape => {}
            _ => {
                hits.insert(line, (shape, via));
            }
        }
    };

    for lit in &strings {
        if let Some((shape, via)) = classify_string(&src, lit, name, &syn) {
            add(lit.open, shape, via);
        }
    }

    let mut unresolved_lines: Vec<usize> = Vec::new();
    let counts_calls = call_family(language).is_some();
    let mut call_lines: Vec<usize> = Vec::new();
    let nb = name.as_bytes();
    let mut occurrences = Vec::new();
    let mut from = 0;
    while let Some(s) = find_sub(&m, from, nb) {
        let e = s + nb.len();
        from = s + 1;
        let left_ok = s == 0 || !is_ident_byte(m[s - 1], &syn);
        let right_ok = e >= m.len()
            || !(is_ident_byte(m[e], &syn) || (syn.ruby && matches!(m[e], b'?' | b'!')));
        if left_ok && right_ok {
            occurrences.push((s, e));
        }
    }
    // A local binding of the name shadows every bare use in the file;
    // qualified uses (`obj.save`) still count. The definition's own name — the
    // first occurrence on a definition line — is not a local, but a parameter
    // of the same name on that line is (`fn append(&mut self, append: bool)`).
    let mut shadowed = false;
    if !syn.ruby {
        let mut def_name_lines: Vec<usize> = Vec::new();
        for (i, &(s, e)) in occurrences.iter().enumerate() {
            if i % 256 == 0 && past() {
                return None;
            }
            let line = src.line_no(s);
            if def_lines.contains(&line) && !def_name_lines.contains(&line) {
                def_name_lines.push(line);
                continue;
            }
            if is_binding(&src, s, e, &syn) {
                shadowed = true;
                break;
            }
        }
    }

    // Lines that define or declare the name (`def name(`, `fn name(`): a call
    // there is the definition's own one-line body, whichever line its node
    // starts on (a decorated Python def starts at the decorator).
    let decl_lines: std::collections::HashSet<usize> = if counts_calls {
        occurrences
            .iter()
            .filter(|&&(s, _)| {
                word_before(&m, s, &syn).is_some_and(|w| NOT_CALL_KEYWORDS.contains(&w.as_str()))
            })
            .map(|&(s, _)| src.line_no(s))
            .collect()
    } else {
        std::collections::HashSet::new()
    };

    for (i, &(s, e)) in occurrences.iter().enumerate() {
        if i % 256 == 0 && past() {
            return None;
        }
        if syn.ruby {
            // `:save` — a symbol, not `Foo::save` and not `a ?b :save`.
            if s >= 1
                && m[s - 1] == b':'
                && (s < 2 || !(m[s - 2] == b':' || is_ident_byte(m[s - 2], &syn)))
            {
                let callee = enclosing_callee(&src, s - 1, &syn)
                    .or_else(|| parenless_callee(&m, s - 1, &syn));
                match callee.as_deref().and_then(classify_callee) {
                    Some(shape) => add(s, shape, callee),
                    None => add(s, Shape::Symbol, None),
                }
            }
            continue;
        }
        if counts_calls && !(shadowed && is_bare(&m, s)) && is_call_site(&src, s, e, &syn, language)
        {
            let line = src.line_no(s);
            // Occurrences come in order, so a repeat is the last one.
            if !def_lines.contains(&line)
                && !decl_lines.contains(&line)
                && call_lines.last() != Some(&line)
            {
                call_lines.push(line);
            }
        }
        if s > 0 && matches!(m[s - 1], b'@' | b'$') {
            continue;
        }
        if shadowed && is_bare(&m, s) {
            continue;
        }
        let mut unresolved = false;
        if is_function_reference(&src, s, e, &syn, qualifiers, &mut unresolved) {
            if unresolved {
                let line = src.line_no(s);
                if !unresolved_lines.contains(&line) {
                    unresolved_lines.push(line);
                }
            } else {
                add(s, Shape::FunctionReference, None);
            }
        }
    }
    unresolved_lines.retain(|l| !hits.contains_key(l));

    Some(SourceScan {
        hits: hits
            .into_iter()
            .map(|(line, (shape, via))| ShapeHit { line, shape, via })
            .collect(),
        unresolved_lines,
        call_lines,
    })
}

/// The languages whose call syntax the scan reads (D#229), by the family a
/// call can come from: a definition in one is called from files of the same
/// family only. `None` for every other language: its calls are not counted,
/// which is not the same as there being none.
pub fn call_family(language: &str) -> Option<&'static str> {
    match language {
        "rust" => Some("rust"),
        "python" => Some("python"),
        "javascript" | "typescript" | "tsx" => Some("js"),
        _ => None,
    }
}

/// The families whose unresolved calls an empty answer lists. Rust only: it
/// is the one language measured (tokio against rust-analyzer). The Python and
/// JS/TS call tables are read by [`scan_calls`] but not disclosed until they
/// are measured on their own corpora, where the 2026-10-07 review found
/// shapes they still miscount (f-strings, top-level calls, TypeScript
/// signatures without a return type).
const DISCLOSED_CALL_FAMILIES: &[&str] = &["rust"];

/// The 1-based lines holding a call of `name` in one source text, skipping
/// `def_lines`; `None` for a language [`call_family`] does not read.
pub fn scan_calls(
    language: &str,
    source: &str,
    name: &str,
    def_lines: &[usize],
) -> Option<Vec<usize>> {
    call_family(language)?;
    scan_source_until(language, source, name, def_lines, None, None).map(|s| s.call_lines)
}

/// Words before a name that make `name(` a definition, not a call.
const NOT_CALL_KEYWORDS: &[&str] = &[
    "def",
    "fn",
    "function",
    "func",
    "fun",
    "class",
    "struct",
    "enum",
    "union",
    "trait",
    "interface",
    "type",
    "impl",
];

/// Words that can open a TypeScript member declaration (`abstract
/// remove(x): void;`).
const MEMBER_MODIFIERS: &[&str] = &[
    "public",
    "private",
    "protected",
    "static",
    "async",
    "abstract",
    "readonly",
    "override",
    "declare",
    "get",
    "set",
];

/// Past a balanced `<…>` opened at `lt` (not counting the `>` of a `->`):
/// the position after its `>`, or `None` when it does not close nearby. A `;`
/// ends the scan only outside `[…]`: an array type holds one (`::<[u8; 4]>`,
/// D#278(2)).
fn past_angle(m: &[u8], lt: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut square = 0usize;
    for (k, &b) in m.iter().enumerate().skip(lt).take(512) {
        match b {
            b'<' => depth += 1,
            b'>' if !(k > 0 && m[k - 1] == b'-') => {
                depth -= 1;
                if depth == 0 {
                    return Some(k + 1);
                }
            }
            b'[' => square += 1,
            b']' => square = square.saturating_sub(1),
            b';' if square == 0 => return None,
            b'{' | b'}' => return None,
            _ => {}
        }
    }
    None
}

/// Inside a Rust attribute: `#[name(…)]`, `#[cfg_attr(x, name(…))]`.
fn in_attribute(src: &Src, pos: usize) -> bool {
    let m = src.m;
    let mut at = pos;
    for _ in 0..8 {
        let Some(o) = src.enclosing_opener(at) else {
            return false;
        };
        if m[o] == b'['
            && o > 0
            && (m[o - 1] == b'#' || (m[o - 1] == b'!' && o > 1 && m[o - 2] == b'#'))
        {
            return true;
        }
        at = o;
    }
    false
}

/// Is the occurrence at `[s, e)` a call of the name: followed by its
/// argument list, past a turbofish (`name::<T>(`) or TypeScript type
/// arguments (`name<T>(`)? Not a definition or declaration (`fn name(`,
/// `def name(`, `function name(`, a JS method `name(…) {`, a TypeScript
/// signature `name(…): T;`), and not a Rust attribute argument.
fn is_call_site(src: &Src, s: usize, e: usize, syn: &Syntax, language: &str) -> bool {
    let m = src.m;
    let rust = language == "rust";
    let ts = matches!(language, "typescript" | "tsx");
    let js = ts || language == "javascript";
    let after = |j: usize| next_non_ws(m, j).map(|(k, _, _)| k);
    let Some(mut j) = after(e) else {
        return false;
    };
    if rust && m[j..].starts_with(b"::<") {
        let Some(k) = past_angle(m, j + 2).and_then(after) else {
            return false;
        };
        j = k;
    } else if ts && m[j] == b'<' {
        let Some(k) = past_angle(m, j).and_then(after) else {
            return false;
        };
        j = k;
    }
    if m[j] != b'(' {
        return false;
    }
    if word_before(m, s, syn).is_some_and(|w| NOT_CALL_KEYWORDS.contains(&w.as_str())) {
        return false;
    }
    if js {
        // `function* name(`
        if let Some((p, b'*')) = prev_non_ws(m, s) {
            if word_before(m, p, syn).as_deref() == Some("function") {
                return false;
            }
        }
        if is_bare(m, s) {
            match src.matching_close(j).and_then(|c| next_non_ws(m, c + 1)) {
                // A method body: `name(x) {`, `async name(x) {`, `{ name(x) {} }`.
                Some((_, b'{', _)) => return false,
                // A signature: `name(x: T): R;` where a statement starts.
                Some((_, b':', _)) => {
                    let starts = match prev_non_ws(m, s) {
                        None => true,
                        Some((_, b';' | b'{' | b'}')) => true,
                        _ => word_before(m, s, syn)
                            .is_some_and(|w| MEMBER_MODIFIERS.contains(&w.as_str())),
                    };
                    if starts {
                        return false;
                    }
                }
                _ => {}
            }
        }
    }
    !(rust && in_attribute(src, s))
}

/// One reported site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundarySite {
    pub file_path: String,
    pub line: usize,
    pub shape: Shape,
    pub via: Option<String>,
}

/// One call of the name in production code (D#229).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallSite {
    pub file_path: String,
    pub line: usize,
    /// The function holding the call has a `calls` edge, at any tier, to a
    /// definition of the name: the graph resolved a call of it there. Such
    /// a call is not listed; an empty answer then says none reached the
    /// definition asked about, or the answer's floor hid it.
    pub resolved: bool,
}

/// The disclosure attached to an empty caller result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Boundaries {
    /// The bare name that was scanned for.
    pub name: String,
    /// Every site found, ordered by (file, line). Rendering caps it.
    pub sites: Vec<BoundarySite>,
    /// Languages of the name's function definitions that have no shape table
    /// (bash, …): files the scan never reads, where the name may well be
    /// dispatched (`trap cleanup EXIT`).
    pub unscanned_languages: Vec<String>,
    /// Production files in a scanned language left unread: over 2 MB, or not
    /// UTF-8 and containing the name.
    pub skipped_files: usize,
    /// Files not reached within [`SCAN_TIME_LIMIT`].
    pub files_past_limit: usize,
    /// Rust lines passing the name as a value through a path whose qualifier
    /// is not known to be its own (`myapp::save`, `inner::save`, `h::save`,
    /// `Store::save`, `<Db as Store>::save`): not sites, and not absences.
    pub unresolved_paths: usize,
    /// Calls of the name in production files of the definitions' language
    /// family, outside test functions and definition lines; `None` when a
    /// function definition of the name is in a language whose calls are not
    /// counted ([`call_family`]).
    pub calls: Option<Vec<CallSite>>,
}

impl Boundaries {
    /// The calls with no resolved target ([`CallSite::resolved`] false);
    /// `None` when calls are not counted for the name.
    pub fn unresolved_calls(&self) -> Option<Vec<&CallSite>> {
        self.calls
            .as_ref()
            .map(|c| c.iter().filter(|s| !s.resolved).collect())
    }

    /// The text block for unresolved calls (empty when there are none).
    fn unresolved_calls_text(&self, indent: &str) -> String {
        let Some(calls) = self.unresolved_calls().filter(|c| !c.is_empty()) else {
            return String::new();
        };
        let mut files: Vec<&str> = calls.iter().map(|c| c.file_path.as_str()).collect();
        files.sort_unstable();
        files.dedup();
        let n = calls.len();
        let mut out = format!(
            "{indent}{n} {} of '{}' in {} {} no resolved target; a caller of this definition may be among them:\n",
            if n == 1 { "call" } else { "calls" },
            self.name,
            files.len(),
            match (files.len() == 1, n == 1) {
                (true, true) => "file has",
                (true, false) => "file have",
                (false, _) => "files have",
            },
        );
        for c in calls.iter().take(BOUNDARY_SITE_CAP) {
            out.push_str(&format!("{indent}  {}:{}\n", c.file_path, c.line));
        }
        if n > BOUNDARY_SITE_CAP {
            out.push_str(&format!("{indent}  … {} more\n", n - BOUNDARY_SITE_CAP));
        }
        out
    }

    /// Whether every file that could hold a site was read.
    pub fn complete(&self) -> bool {
        self.unscanned_languages.is_empty()
            && self.skipped_files == 0
            && self.files_past_limit == 0
            && self.unresolved_paths == 0
    }

    /// The follow-up that shows every textual occurrence, comments and tests
    /// included — the superset this scan narrowed.
    pub fn next_command(&self) -> String {
        let quoted = if self
            .name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            self.name.clone()
        } else {
            format!("'{}'", self.name.replace('\'', "'\\''"))
        };
        format!("code-graph-mcp grep -w -F {quoted}")
    }

    /// What was not read, for the text answer: `bash files, 2 files past the
    /// scan time limit`.
    fn unscanned_text(&self) -> String {
        let plural =
            |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let mut parts = Vec::new();
        if !self.unscanned_languages.is_empty() {
            parts.push(format!("{} files", self.unscanned_languages.join(", ")));
        }
        if self.skipped_files > 0 {
            parts.push(format!(
                "{} over 2 MB or not UTF-8",
                plural(self.skipped_files, "file", "files")
            ));
        }
        if self.files_past_limit > 0 {
            parts.push(format!(
                "{} past the scan time limit",
                plural(self.files_past_limit, "file", "files")
            ));
        }
        if self.unresolved_paths > 0 {
            parts.push(format!(
                "{} with an unrecognized qualifier",
                plural(self.unresolved_paths, "Rust path", "Rust paths")
            ));
        }
        parts.join(", ")
    }

    /// Additive JSON field `boundaries`.
    pub fn to_json(&self) -> serde_json::Value {
        let sites: Vec<serde_json::Value> = self
            .sites
            .iter()
            .take(BOUNDARY_SITE_CAP)
            .map(|s| {
                let mut v = serde_json::json!({
                    "file_path": s.file_path,
                    "line": s.line,
                    "shape": s.shape.as_str(),
                });
                if let Some(via) = &s.via {
                    v["via"] = serde_json::json!(via);
                }
                v
            })
            .collect();
        let mut v = if self.sites.is_empty() {
            serde_json::json!({ "total": 0, "sites": sites })
        } else {
            serde_json::json!({
                "total": self.sites.len(),
                "sites": sites,
                "note": "name used in dynamic-dispatch shapes the static graph does not follow; not edges",
                "next": self.next_command(),
            })
        };
        // Only a list is disclosed; with none, the 0.166.0 shape (see
        // `render_text`).
        if let Some(calls) = self.unresolved_calls().filter(|c| !c.is_empty()) {
            let mut u = serde_json::json!({ "total": calls.len() });
            let in_resolved = self.calls.as_ref().map_or(0, |c| c.len() - calls.len());
            if in_resolved > 0 {
                u["in_resolved_functions"] = serde_json::json!(in_resolved);
            }
            {
                let mut files: Vec<&str> = calls.iter().map(|c| c.file_path.as_str()).collect();
                files.sort_unstable();
                files.dedup();
                u["files"] = serde_json::json!(files.len());
                u["sites"] = calls
                    .iter()
                    .take(BOUNDARY_SITE_CAP)
                    .map(|c| serde_json::json!({ "file_path": c.file_path, "line": c.line }))
                    .collect();
                u["note"] = serde_json::json!(
                    "calls of the name with no resolved target; a caller of this definition may be among them"
                );
                v["next"] = serde_json::json!(self.next_command());
            }
            v["unresolved_calls"] = u;
        }
        if !self.complete() {
            let mut ns = serde_json::Map::new();
            if !self.unscanned_languages.is_empty() {
                ns.insert(
                    "languages".into(),
                    serde_json::json!(self.unscanned_languages),
                );
            }
            if self.skipped_files > 0 {
                ns.insert(
                    "skipped_files".into(),
                    serde_json::json!(self.skipped_files),
                );
            }
            if self.files_past_limit > 0 {
                ns.insert(
                    "files_past_time_limit".into(),
                    serde_json::json!(self.files_past_limit),
                );
            }
            if self.unresolved_paths > 0 {
                ns.insert(
                    "unresolved_paths".into(),
                    serde_json::json!(self.unresolved_paths),
                );
            }
            v["not_scanned"] = serde_json::Value::Object(ns);
            v["next"] = serde_json::json!(self.next_command());
        }
        v
    }

    /// Text block printed after an empty result, each line prefixed by `indent`.
    pub fn render_text<W: std::io::Write>(&self, out: &mut W, indent: &str) -> std::io::Result<()> {
        let calls_text = self.unresolved_calls_text(indent);
        if self.sites.is_empty() {
            // Nothing listed: the 0.166.0 answer. No line claims there is no
            // call — the scan skips a bare call in a file that binds the
            // name, and the definition's own line (pre-tag review round 2).
            if self.complete() {
                writeln!(
                    out,
                    "{indent}(no dynamic-dispatch site names '{}')",
                    self.name
                )?;
                if calls_text.is_empty() {
                    return Ok(());
                }
                out.write_all(calls_text.as_bytes())?;
                return writeln!(out, "{indent}  next: {}", self.next_command());
            }
            writeln!(
                out,
                "{indent}(no dynamic-dispatch site names '{}' in the files scanned; not scanned: {})",
                self.name,
                self.unscanned_text()
            )?;
            out.write_all(calls_text.as_bytes())?;
            return writeln!(out, "{indent}  next: {}", self.next_command());
        }
        writeln!(
            out,
            "{indent}{} dynamic-dispatch site(s) name '{}' (not graph edges):",
            self.sites.len(),
            self.name
        )?;
        for s in self.sites.iter().take(BOUNDARY_SITE_CAP) {
            match &s.via {
                Some(via) => writeln!(
                    out,
                    "{indent}  {}:{}  {} ({via})",
                    s.file_path,
                    s.line,
                    s.shape.label()
                )?,
                None => writeln!(
                    out,
                    "{indent}  {}:{}  {}",
                    s.file_path,
                    s.line,
                    s.shape.label()
                )?,
            }
        }
        if self.sites.len() > BOUNDARY_SITE_CAP {
            writeln!(
                out,
                "{indent}  … {} more",
                self.sites.len() - BOUNDARY_SITE_CAP
            )?;
        }
        out.write_all(calls_text.as_bytes())?;
        if !self.complete() {
            writeln!(out, "{indent}  not scanned: {}", self.unscanned_text())?;
        }
        writeln!(out, "{indent}  next: {}", self.next_command())
    }
}

/// Is this a file the scan reads? Production code in a language with a
/// shape table.
fn scannable(path: &str, language: Option<&str>) -> bool {
    language.and_then(syntax_for).is_some()
        && !crate::domain::is_test_path(path)
        && !path.ends_with("_spec.rb")
}

/// Scan every indexed production file for `name`, skipping the given
/// `(file, line)` definition sites. Files are read from disk; one deleted
/// since indexing is skipped silently, one too large or not UTF-8 is counted.
pub fn scan_project(
    conn: &Connection,
    project_root: &Path,
    name: &str,
    exclude: &[(String, usize)],
) -> Result<Boundaries> {
    scan_project_with(
        conn,
        project_root,
        name,
        exclude,
        &[],
        None,
        None,
        SCAN_TIME_LIMIT,
    )
}

/// [`scan_project`], told the files holding the name's function definitions
/// (a language there without a shape table is reported as not scanned), the
/// path qualifiers that name one of them, and the scan's time limit.
#[allow(clippy::too_many_arguments)] // the scan's inputs, each a fact about the definitions
fn scan_project_with(
    conn: &Connection,
    project_root: &Path,
    name: &str,
    exclude: &[(String, usize)],
    def_files: &[String],
    qualifiers: Option<&[String]>,
    call_families: Option<&[&'static str]>,
    time_limit: Duration,
) -> Result<Boundaries> {
    let deadline = Instant::now() + time_limit;
    let files: Vec<(String, Option<String>)> = {
        let mut stmt = conn.prepare("SELECT path, language FROM files ORDER BY path")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut out = Boundaries {
        name: name.to_string(),
        sites: Vec::new(),
        unscanned_languages: Vec::new(),
        skipped_files: 0,
        files_past_limit: 0,
        unresolved_paths: 0,
        calls: call_families.map(|_| Vec::new()),
    };
    // Call lines per file, classified against the graph below.
    let mut call_lines: Vec<(String, Vec<usize>)> = Vec::new();
    for (path, language) in files {
        if !scannable(&path, language.as_deref()) {
            if def_files.contains(&path) && language.as_deref().and_then(syntax_for).is_none() {
                let lang = language.unwrap_or_else(|| "unknown".to_string());
                if !out.unscanned_languages.contains(&lang) {
                    out.unscanned_languages.push(lang);
                }
            }
            continue;
        }
        if out.files_past_limit > 0 || Instant::now() >= deadline {
            out.files_past_limit += 1;
            continue;
        }
        let abs = project_root.join(&path);
        match std::fs::metadata(&abs) {
            Ok(md) if md.len() <= MAX_SCAN_BYTES => {}
            Ok(_) => {
                out.skipped_files += 1;
                continue;
            }
            Err(_) => continue,
        }
        let Ok(bytes) = std::fs::read(&abs) else {
            continue;
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            if find_sub(&bytes, 0, name.as_bytes()).is_some() {
                out.skipped_files += 1;
            }
            continue;
        };
        if !text.contains(name) {
            continue;
        }
        let def_lines: Vec<usize> = exclude
            .iter()
            .filter(|(f, _)| *f == path)
            .map(|(_, l)| *l)
            .collect();
        let language = language.as_deref().unwrap_or_default();
        let Some(scan) =
            scan_source_until(language, text, name, &def_lines, qualifiers, Some(deadline))
        else {
            out.files_past_limit += 1;
            continue;
        };
        out.unresolved_paths += scan
            .unresolved_lines
            .iter()
            .filter(|l| !def_lines.contains(l))
            .count();
        for hit in scan.hits {
            if def_lines.contains(&hit.line) {
                continue;
            }
            out.sites.push(BoundarySite {
                file_path: path.clone(),
                line: hit.line,
                shape: hit.shape,
                via: hit.via,
            });
        }
        let in_family = call_family(language)
            .is_some_and(|f| call_families.is_some_and(|families| families.contains(&f)));
        if in_family && !scan.call_lines.is_empty() {
            call_lines.push((path.clone(), scan.call_lines));
        }
    }
    if out.calls.is_some() {
        let calls = classify_calls(conn, name, call_lines, deadline, &mut out.files_past_limit)?;
        out.calls = Some(calls);
    }
    Ok(out)
}

/// Each call line placed in the innermost function holding it: a call in a
/// test function is dropped (tests are not production callers), the rest
/// flagged [`CallSite::resolved`] when that function has a call edge to a
/// definition of the name. A line outside every function belongs to the
/// innermost module node holding it (a top-level call's edge leaves from
/// there). When several functions hold the line at the same depth (two
/// one-line functions), the call is resolved only if all of them are, and
/// test only if all of them are: which one holds it is unknown, and a call
/// wrongly listed is the cheaper error than a zero wrongly backed.
///
/// The tokio measurement behind this rule is in
/// `tasks/specs/d229-zero-answer-disclosure.md`. It is per function, not per
/// call (an edge carries no line), so a function with one resolved call of
/// the name counts its other calls of it as resolved too; the zero line says
/// only that much.
///
/// Files left when `deadline` passes are not classified: their calls are
/// dropped and the files counted in `files_past_limit`.
fn classify_calls(
    conn: &Connection,
    name: &str,
    call_lines: Vec<(String, Vec<usize>)>,
    deadline: Instant,
    files_past_limit: &mut usize,
) -> Result<Vec<CallSite>> {
    if call_lines.is_empty() {
        return Ok(Vec::new());
    }
    let resolving: std::collections::HashSet<i64> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT e.source_id FROM edges e JOIN nodes t ON t.id = e.target_id \
             WHERE e.relation = 'calls' AND t.name = ?1",
        )?;
        let rows = stmt.query_map([name], |r| r.get::<_, i64>(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut stmt = conn.prepare(
        "SELECT n.id, n.type, n.start_line, n.end_line, n.is_test FROM nodes n \
         JOIN files f ON f.id = n.file_id WHERE f.path = ?1",
    )?;
    let mut out = Vec::new();
    let total = call_lines.len();
    for (k, (path, lines)) in call_lines.into_iter().enumerate() {
        if Instant::now() >= deadline {
            *files_past_limit += total - k;
            break;
        }
        // (start, end, id, is_test), functions and modules apart, by start.
        let mut fns: Vec<(i64, i64, i64, bool)> = Vec::new();
        let mut mods: Vec<(i64, i64, i64, bool)> = Vec::new();
        for row in stmt.query_map([&path], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, bool>(4)?,
            ))
        })? {
            let (id, ty, start, end, test) = row?;
            if crate::domain::is_function_node_type(&ty) {
                fns.push((start, end, id, test));
            } else if ty == "module" {
                mods.push((start, end, id, test));
            }
        }
        fns.sort_unstable();
        mods.sort_unstable();
        let first = out.len();
        for (j, line) in lines.into_iter().enumerate() {
            if j % 1024 == 1023 && Instant::now() >= deadline {
                out.truncate(first);
                *files_past_limit += total - k;
                return Ok(out);
            }
            let l = line as i64;
            let holders = innermost(&fns, l);
            let holders = if holders.is_empty() {
                innermost(&mods, l)
            } else {
                holders
            };
            if !holders.is_empty() && holders.iter().all(|h| h.3) {
                continue;
            }
            out.push(CallSite {
                file_path: path.clone(),
                line,
                resolved: !holders.is_empty() && holders.iter().all(|h| resolving.contains(&h.2)),
            });
        }
    }
    Ok(out)
}

/// The intervals (sorted by start) holding `line` that start last: the
/// innermost, several when they start on the same line. Walks back from the
/// last start at or before `line` to the first that holds it.
fn innermost(spans: &[(i64, i64, i64, bool)], line: i64) -> Vec<(i64, i64, i64, bool)> {
    let upto = spans.partition_point(|s| s.0 <= line);
    let Some(i) = spans[..upto].iter().rposition(|s| s.1 >= line) else {
        return Vec::new();
    };
    let best = spans[i].0;
    // The block starting on `best` (spans are sorted by start).
    let lo = spans[..i].partition_point(|s| s.0 < best);
    spans[lo..=i]
        .iter()
        .filter(|s| s.1 >= line)
        .copied()
        .collect()
}

/// Last segment of a possibly qualified symbol (`Store::save`, `Store.save`).
pub fn bare_name(symbol: &str) -> &str {
    let tail = symbol.rsplit("::").next().unwrap_or(symbol);
    tail.rsplit('.').next().unwrap_or(tail)
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

/// Boundaries for an empty caller result on `symbol`, or `None` when the
/// disclosure does not apply: the name is not an identifier, or no definition
/// of it is a function or method (a type or constant is not dispatched to).
/// `near`: the node ids of the definition(s) asked about, which the
/// unresolved calls are listed nearest to; empty for every function
/// definition of the name.
pub fn for_empty_result(
    conn: &Connection,
    project_root: &Path,
    symbol: &str,
    near: &[i64],
) -> Result<Option<Boundaries>> {
    let name = bare_name(symbol);
    if !is_identifier(name) {
        return Ok(None);
    }
    let defs = crate::storage::queries::get_nodes_with_files_by_name(conn, name)?;
    if !defs
        .iter()
        .any(|d| crate::domain::is_function_node_type(&d.node.node_type))
    {
        return Ok(None);
    }
    let exclude: Vec<(String, usize)> = defs
        .iter()
        .map(|d| (d.file_path.clone(), d.node.start_line.max(0) as usize))
        .collect();
    let fn_defs: Vec<_> = defs
        .iter()
        .filter(|d| crate::domain::is_function_node_type(&d.node.node_type))
        .collect();
    let def_files: Vec<String> = fn_defs.iter().map(|d| d.file_path.clone()).collect();
    let qualifiers = path_qualifiers(
        fn_defs
            .iter()
            .map(|d| (d.file_path.as_str(), d.node.qualified_name.as_deref())),
    );
    // Calls are counted only when every function definition is in a language
    // whose calls the answer discloses; one elsewhere could be called from
    // files it does not read.
    let families: Option<Vec<&'static str>> = fn_defs
        .iter()
        .map(|d| {
            d.language
                .as_deref()
                .and_then(call_family)
                .filter(|f| DISCLOSED_CALL_FAMILIES.contains(f))
        })
        .collect();
    let mut out = scan_project_with(
        conn,
        project_root,
        name,
        &exclude,
        &def_files,
        Some(&qualifiers),
        families.as_deref(),
        SCAN_TIME_LIMIT,
    )?;
    if let Some(calls) = out.calls.as_mut() {
        // Nearest the definition asked about first: the answer lists five,
        // and in path order tokio's `examples/` and `tokio-util/` calls of
        // `remove` came before every `tokio/src/` one.
        let asked: Vec<&str> = fn_defs
            .iter()
            .filter(|d| near.contains(&d.node.id))
            .map(|d| d.file_path.as_str())
            .collect();
        let anchors: Vec<&str> = if asked.is_empty() {
            def_files.iter().map(String::as_str).collect()
        } else {
            asked
        };
        let shared = |file: &str| {
            anchors
                .iter()
                .map(|d| {
                    d.split('/')
                        .zip(file.split('/'))
                        .take_while(|(a, b)| a == b)
                        .count()
                })
                .max()
                .unwrap_or(0)
        };
        calls.sort_by(|a, b| {
            shared(&b.file_path)
                .cmp(&shared(&a.file_path))
                .then_with(|| a.file_path.cmp(&b.file_path))
                .then(a.line.cmp(&b.line))
        });
    }
    Ok(Some(out))
}

/// The path segments that can stand right before a definition's name and
/// still mean it: its type (`Store` of `Store::save`), its module (the file
/// stem, or the directory of a `mod.rs` / `lib.rs` / `main.rs`), and the
/// relative ones (`Self`, `self`, `super`, `crate`).
fn path_qualifiers<'a>(defs: impl Iterator<Item = (&'a str, Option<&'a str>)>) -> Vec<String> {
    let mut out: Vec<String> = ["Self", "self", "super", "crate"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut push = |q: &str| {
        if !q.is_empty() && !out.iter().any(|o| o == q) {
            out.push(q.to_string());
        }
    };
    for (file, qualified) in defs {
        if let Some(q) = qualified {
            let mut segs: Vec<&str> = q.split("::").flat_map(|p| p.split('.')).collect();
            segs.pop();
            if let Some(owner) = segs.last() {
                push(owner);
            }
        }
        let path = Path::new(file);
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if matches!(stem, "mod" | "lib" | "main") {
            let dir = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                .unwrap_or("");
            push(dir);
        } else {
            push(stem);
        }
    }
    out
}

#[cfg(test)]
mod tests;
