//! A reader for Lua *table literals*, as KOReader writes them (`frontend/dump.lua`).
//!
//! It never executes Lua. The grammar is the data subset of Lua 5.1-5.4 expressions:
//! an optional `return`, then one value: `nil`, `true`, `false`, numbers (decimal,
//! hex, exponents and LuaJIT's `inf`, `-inf`, `nan`, `-nan`), strings (short strings with
//! every Lua escape, long brackets `[[ ]]` / `[==[ ]==]`) and table constructors with
//! `[key] = value`, `name = value` and positional fields separated by `,` or `;`. Line
//! and block comments are skipped (KOReader writes `nil --[[ LOOP: ... ]]` for cycles).
//! Anything else (identifiers, calls, operators, concatenation) is an error, so a
//! file can never make the reader do anything but build values.
//!
//! Strings are byte strings in Lua; they are decoded as UTF-8 at the end, with invalid
//! sequences replaced (`String::from_utf8_lossy`). Input size, value count and nesting
//! depth are limited ([`MAX_INPUT`], [`MAX_VALUES`], [`MAX_DEPTH`]) so that any accepted
//! input parses in memory a Kindle has to spare; malformed or oversized input returns an
//! error and never panics.

use std::collections::BTreeMap;
use std::fmt;

/// Largest input accepted, in bytes (8 MiB).
///
/// KOReader's `dump.lua` writes one value per line with indentation: about 450 bytes
/// per EPUB annotation and 900 bytes per PDF annotation with two boxes (the fixtures in
/// `tests/fixtures`), so 8 MiB holds roughly 9 000 PDF or 18 000 EPUB annotations.
pub const MAX_INPUT: usize = 8 << 20;

/// Most values (of any type, at any depth, `nil` included) one input may hold.
///
/// Each parsed table entry costs 32 bytes on 32-bit ARM and 56 bytes on a 64-bit host
/// (measured), plus up to half again while a table's vector grows: 1 000 000 values is
/// at most about 50 MB on the Kindle and 90 MB on the host. Without this limit 8 MiB of
/// `1,` would be 4 million entries. Real files hold one value per 12 bytes or more, so a
/// file under [`MAX_INPUT`] that KOReader wrote stays under this limit.
pub const MAX_VALUES: usize = 1_000_000;

/// Deepest table nesting accepted.
pub const MAX_DEPTH: usize = 100;

/// A Lua value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `nil`.
    Nil,
    /// `true` / `false`.
    Bool(bool),
    /// An integer literal that fits in 64 bits.
    Int(i64),
    /// Any other number.
    Num(f64),
    /// A string (decoded as UTF-8, lossy).
    Str(String),
    /// A table.
    Table(Table),
}

/// A table key. Float keys with an integer value are stored as [`Key::Int`], as in Lua.
#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    /// Integer key (array part).
    Int(i64),
    /// Non-integer number key.
    Num(f64),
    /// String key.
    Str(String),
    /// Boolean key.
    Bool(bool),
}

/// A table: entries in source order. Later entries with an equal key win, as in Lua.
/// Fields assigned `nil` are left out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Table {
    /// Entries in source order.
    pub entries: Vec<(Key, Value)>,
}

impl Table {
    /// Value of string key `key`.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries.iter().rev().find(|(k, _)| matches!(k, Key::Str(s) if s == key)).map(|(_, v)| v)
    }

    /// Value of integer key `i`.
    pub fn get_index(&self, i: i64) -> Option<&Value> {
        self.entries.iter().rev().find(|(k, _)| *k == Key::Int(i)).map(|(_, v)| v)
    }

    /// String value of `key`.
    pub fn str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }

    /// Numeric value of `key`.
    pub fn num(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(Value::as_f64)
    }

    /// Integer value of `key` (floats with an integer value count).
    pub fn int(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(Value::as_i64)
    }

    /// Boolean value of `key`.
    pub fn bool(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(Value::as_bool)
    }

    /// Table value of `key`.
    pub fn table(&self, key: &str) -> Option<&Table> {
        self.get(key).and_then(Value::as_table)
    }

    /// The sequence `t[1], t[2], ...` up to the first missing index (Lua's `ipairs`).
    pub fn seq(&self) -> Vec<&Value> {
        let ints = self.int_map();
        let mut out = Vec::new();
        let mut i = 1;
        while let Some(v) = ints.get(&i) {
            out.push(*v);
            i += 1;
        }
        out
    }

    /// All integer-keyed values in key order (holes allowed), like KOReader's
    /// `highlight` table keyed by page.
    pub fn int_entries(&self) -> Vec<(i64, &Value)> {
        self.int_map().into_iter().collect()
    }

    /// Number of entries (keys counted once).
    pub fn len(&self) -> usize {
        /// A key that can be hashed. The parser stores no NaN and no integral number as
        /// [`Key::Num`], but `entries` is public: a hand-built -0.0 equals 0.0 (`+ 0.0`
        /// makes their bits equal) and a NaN equals nothing, so each NaN counts on its own.
        #[derive(PartialEq, Eq, Hash)]
        enum K<'a> {
            Int(i64),
            Num(u64),
            Str(&'a str),
            Bool(bool),
        }
        let mut nans = 0;
        let mut keys = std::collections::HashSet::with_capacity(self.entries.len());
        for (k, _) in &self.entries {
            keys.insert(match k {
                Key::Int(i) => K::Int(*i),
                Key::Num(n) if n.is_nan() => {
                    nans += 1;
                    continue;
                }
                Key::Num(n) => K::Num((n + 0.0).to_bits()),
                Key::Str(s) => K::Str(s),
                Key::Bool(b) => K::Bool(*b),
            });
        }
        keys.len() + nans
    }

    /// True if the table has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn int_map(&self) -> BTreeMap<i64, &Value> {
        let mut m = BTreeMap::new();
        for (k, v) in &self.entries {
            if let Key::Int(i) = k {
                m.insert(*i, v);
            }
        }
        m
    }
}

impl Value {
    /// The string, if this is a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The number as `f64`, if this is a number.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Num(n) => Some(*n),
            _ => None,
        }
    }

    /// The number as `i64`, if it is an integer (or a float with an integer value).
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Num(n) if n.fract() == 0.0 && n.abs() < 9.2e18 => Some(*n as i64),
            _ => None,
        }
    }

    /// The boolean, if this is a boolean.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// The table, if this is a table.
    pub fn as_table(&self) -> Option<&Table> {
        match self {
            Value::Table(t) => Some(t),
            _ => None,
        }
    }

    /// Converts to JSON (tables with keys 1..n become arrays, other tables objects with
    /// string keys; NaN and infinities become `null`).
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::Value as J;
        match self {
            Value::Nil => J::Null,
            Value::Bool(b) => J::Bool(*b),
            Value::Int(i) => J::from(*i),
            Value::Num(n) => serde_json::Number::from_f64(*n).map(J::Number).unwrap_or(J::Null),
            Value::Str(s) => J::String(s.clone()),
            Value::Table(t) => {
                let seq = t.seq();
                if !t.is_empty() && seq.len() == t.len() {
                    J::Array(seq.into_iter().map(Value::to_json).collect())
                } else {
                    let mut m = serde_json::Map::new();
                    for (k, v) in &t.entries {
                        let key = match k {
                            Key::Int(i) => i.to_string(),
                            Key::Num(n) => n.to_string(),
                            Key::Str(s) => s.clone(),
                            Key::Bool(b) => b.to_string(),
                        };
                        m.insert(key, v.to_json());
                    }
                    J::Object(m)
                }
            }
        }
    }
}

/// A parse error with its position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LuaError {
    /// 1-based line.
    pub line: usize,
    /// Byte offset.
    pub offset: usize,
    /// What went wrong.
    pub msg: String,
}

impl fmt::Display for LuaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.msg)
    }
}

impl std::error::Error for LuaError {}

/// Parses a Lua chunk of the form `[return] <value> [;]`.
pub fn parse(input: &[u8]) -> Result<Value, LuaError> {
    let mut p = Parser { s: input, i: 0, depth: 0, values: 0 };
    if input.len() > MAX_INPUT {
        return Err(p.err(format!("input of {} bytes is larger than {MAX_INPUT}", input.len())));
    }
    // A UTF-8 byte order mark and a `#!` first line are skipped like Lua's loader does.
    if p.s.starts_with(b"\xEF\xBB\xBF") {
        p.i = 3;
    }
    if p.s[p.i..].starts_with(b"#") {
        while p.i < p.s.len() && p.s[p.i] != b'\n' {
            p.i += 1;
        }
    }
    p.skip_space()?;
    if p.peek_word() == Some(b"return") {
        p.i += 6;
        p.skip_space()?;
    }
    let v = p.value()?;
    p.skip_space()?;
    if p.peek() == Some(b';') {
        p.i += 1;
        p.skip_space()?;
    }
    if p.i != p.s.len() {
        return Err(p.err("unexpected text after the value".into()));
    }
    Ok(v)
}

/// Parses a chunk that must evaluate to a table.
pub fn parse_table(input: &[u8]) -> Result<Table, LuaError> {
    match parse(input)? {
        Value::Table(t) => Ok(t),
        other => Err(LuaError { line: 1, offset: 0, msg: format!("expected a table, found {}", type_name(&other)) }),
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Nil => "nil",
        Value::Bool(_) => "boolean",
        Value::Int(_) | Value::Num(_) => "number",
        Value::Str(_) => "string",
        Value::Table(_) => "table",
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    depth: usize,
    /// Values parsed so far (bounded by [`MAX_VALUES`]).
    values: usize,
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

impl Parser<'_> {
    fn err(&self, msg: String) -> LuaError {
        let at = self.i.min(self.s.len());
        let line = 1 + self.s[..at].iter().filter(|&&c| c == b'\n').count();
        LuaError { line, offset: at, msg }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn peek_at(&self, n: usize) -> Option<u8> {
        self.s.get(self.i + n).copied()
    }

    /// The identifier starting at the cursor, if any.
    fn peek_word(&self) -> Option<&[u8]> {
        let start = self.i;
        if !self.peek().is_some_and(is_ident_start) {
            return None;
        }
        let mut end = start;
        while end < self.s.len() && is_ident(self.s[end]) {
            end += 1;
        }
        Some(&self.s[start..end])
    }

    /// Skips white space and comments.
    fn skip_space(&mut self) -> Result<(), LuaError> {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n' | b'\x0b' | b'\x0c') => self.i += 1,
                Some(b'-') if self.peek_at(1) == Some(b'-') => {
                    self.i += 2;
                    if let Some(level) = self.long_bracket_level() {
                        self.long_body(level, "comment")?;
                    } else {
                        while let Some(c) = self.peek() {
                            if c == b'\n' {
                                break;
                            }
                            self.i += 1;
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    /// At `[` `=`* `[`: consumes the opener and returns the level (number of `=`).
    fn long_bracket_level(&mut self) -> Option<usize> {
        if self.peek() != Some(b'[') {
            return None;
        }
        let mut j = self.i + 1;
        while self.s.get(j) == Some(&b'=') {
            j += 1;
        }
        if self.s.get(j) == Some(&b'[') {
            let level = j - self.i - 1;
            self.i = j + 1;
            Some(level)
        } else {
            None
        }
    }

    /// Body of a long string or comment after its opener; returns the bytes with line
    /// ends normalized to `\n` and a first newline dropped (Lua's rules).
    fn long_body(&mut self, level: usize, what: &str) -> Result<Vec<u8>, LuaError> {
        let mut out = Vec::new();
        // A newline right after the opener is not part of the string.
        self.skip_newline();
        loop {
            match self.peek() {
                None => return Err(self.err(format!("unfinished long {what}"))),
                Some(b']') => {
                    let mut j = self.i + 1;
                    while self.s.get(j) == Some(&b'=') {
                        j += 1;
                    }
                    if j - self.i - 1 == level && self.s.get(j) == Some(&b']') {
                        self.i = j + 1;
                        return Ok(out);
                    }
                    out.push(b']');
                    self.i += 1;
                }
                Some(b'\r' | b'\n') => {
                    self.skip_newline();
                    out.push(b'\n');
                }
                Some(c) => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }

    /// Skips one line break (`\n`, `\r`, `\r\n` or `\n\r`), if the cursor is on one.
    fn skip_newline(&mut self) {
        if let Some(c @ (b'\r' | b'\n')) = self.peek() {
            self.i += 1;
            if let Some(d @ (b'\r' | b'\n')) = self.peek() {
                if d != c {
                    self.i += 1;
                }
            }
        }
    }

    fn value(&mut self) -> Result<Value, LuaError> {
        self.values += 1;
        if self.values > MAX_VALUES {
            return Err(self.err(format!("more than {MAX_VALUES} values")));
        }
        match self.peek() {
            None => Err(self.err("expected a value, found end of input".into())),
            Some(b'{') => self.table().map(Value::Table),
            Some(b'"' | b'\'') => self.short_string().map(|b| Value::Str(String::from_utf8_lossy(&b).into_owned())),
            Some(b'[') => match self.long_bracket_level() {
                Some(level) => {
                    let b = self.long_body(level, "string")?;
                    Ok(Value::Str(String::from_utf8_lossy(&b).into_owned()))
                }
                None => Err(self.err("unexpected '['".into())),
            },
            Some(b'-') => {
                self.i += 1;
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return Err(self.err(format!("more than {MAX_DEPTH} nested operators")));
                }
                self.skip_space()?;
                let v = match self.value()? {
                    Value::Int(i) => i.checked_neg().map(Value::Int).unwrap_or(Value::Num(-(i as f64))),
                    Value::Num(n) => Value::Num(-n),
                    other => return Err(self.err(format!("cannot negate a {}", type_name(&other)))),
                };
                self.depth -= 1;
                Ok(v)
            }
            Some(c) if c.is_ascii_digit() || (c == b'.' && self.peek_at(1).is_some_and(|d| d.is_ascii_digit())) => self.number(),
            Some(c) if is_ident_start(c) => {
                let word = self.peek_word().unwrap_or_default().to_vec();
                let v = match &word[..] {
                    b"nil" => Value::Nil,
                    b"true" => Value::Bool(true),
                    b"false" => Value::Bool(false),
                    // LuaJIT's tostring() of infinities and NaN, written by dump.lua.
                    b"inf" => Value::Num(f64::INFINITY),
                    b"nan" => Value::Num(f64::NAN),
                    _ => {
                        return Err(self.err(format!(
                            "'{}' is not a literal (identifiers, calls and expressions are refused)",
                            String::from_utf8_lossy(&word)
                        )))
                    }
                };
                self.i += word.len();
                Ok(v)
            }
            Some(c) => Err(self.err(format!("unexpected character {:?}", c as char))),
        }
    }

    fn number(&mut self) -> Result<Value, LuaError> {
        let start = self.i;
        if self.peek() == Some(b'0') && matches!(self.peek_at(1), Some(b'x' | b'X')) {
            self.i += 2;
            return self.hex_number(start);
        }
        let mut is_float = false;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                self.i += 1;
            } else if c == b'.' {
                is_float = true;
                self.i += 1;
            } else if c == b'e' || c == b'E' {
                is_float = true;
                self.i += 1;
                if matches!(self.peek(), Some(b'+' | b'-')) {
                    self.i += 1;
                }
            } else {
                break;
            }
        }
        if self.peek().is_some_and(is_ident) {
            return Err(self.err("malformed number".into()));
        }
        let text = std::str::from_utf8(&self.s[start..self.i]).map_err(|_| self.err("malformed number".into()))?;
        if !is_float {
            if let Ok(i) = text.parse::<i64>() {
                return Ok(Value::Int(i));
            }
        }
        text.parse::<f64>().map(Value::Num).map_err(|_| self.err(format!("malformed number {text:?}")))
    }

    fn hex_number(&mut self, start: usize) -> Result<Value, LuaError> {
        let mut mantissa: u64 = 0;
        let mut value: f64 = 0.0;
        let mut exp: i64 = 0;
        let mut any = false;
        let mut is_float = false;
        let mut after_dot = false;
        while let Some(c) = self.peek() {
            if let Some(d) = (c as char).to_digit(16) {
                any = true;
                mantissa = mantissa.wrapping_mul(16).wrapping_add(d as u64);
                value = value * 16.0 + d as f64;
                if after_dot {
                    exp -= 4;
                }
                self.i += 1;
            } else if c == b'.' && !after_dot {
                is_float = true;
                after_dot = true;
                self.i += 1;
            } else {
                break;
            }
        }
        if matches!(self.peek(), Some(b'p' | b'P')) {
            is_float = true;
            self.i += 1;
            let neg = self.peek() == Some(b'-');
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            let mut e: i64 = 0;
            let mut digits = false;
            while let Some(c) = self.peek().filter(u8::is_ascii_digit) {
                e = (e * 10 + (c - b'0') as i64).min(100_000);
                digits = true;
                self.i += 1;
            }
            if !digits {
                return Err(self.err("malformed hex exponent".into()));
            }
            exp += if neg { -e } else { e };
        }
        if !any || self.peek().is_some_and(is_ident) {
            self.i = start;
            return Err(self.err("malformed hex number".into()));
        }
        if is_float {
            Ok(Value::Num(value * 2f64.powi(exp.clamp(-2000, 2000) as i32)))
        } else {
            // Lua wraps hex integers modulo 2^64.
            Ok(Value::Int(mantissa as i64))
        }
    }

    fn short_string(&mut self) -> Result<Vec<u8>, LuaError> {
        let quote = self.s[self.i];
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let Some(c) = self.peek() else { return Err(self.err("unfinished string".into())) };
            match c {
                _ if c == quote => {
                    self.i += 1;
                    return Ok(out);
                }
                b'\n' | b'\r' => return Err(self.err("unfinished string (line break)".into())),
                b'\\' => {
                    self.i += 1;
                    self.escape(&mut out)?;
                }
                _ => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }

    fn escape(&mut self, out: &mut Vec<u8>) -> Result<(), LuaError> {
        /// One-character escapes and the byte each stands for.
        const SIMPLE: [(u8, u8); 10] =
            [(b'a', 7), (b'b', 8), (b'f', 12), (b'n', b'\n'), (b'r', b'\r'), (b't', b'\t'), (b'v', 11), (b'\\', b'\\'), (b'"', b'"'), (b'\'', b'\'')];
        let Some(c) = self.peek() else { return Err(self.err("unfinished escape".into())) };
        if let Some(&(_, b)) = SIMPLE.iter().find(|e| e.0 == c) {
            out.push(b);
            self.i += 1;
            return Ok(());
        }
        match c {
            b'\n' | b'\r' => {
                self.skip_newline();
                out.push(b'\n');
            }
            b'x' => {
                self.i += 1;
                let mut v = 0u8;
                for _ in 0..2 {
                    let d = self.peek().and_then(|c| (c as char).to_digit(16)).ok_or_else(|| self.err("bad \\x escape".into()))?;
                    v = v * 16 + d as u8;
                    self.i += 1;
                }
                out.push(v);
            }
            b'z' => {
                self.i += 1;
                while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n' | b'\x0b' | b'\x0c')) {
                    self.i += 1;
                }
            }
            b'u' => {
                self.i += 1;
                if self.peek() != Some(b'{') {
                    return Err(self.err("bad \\u escape".into()));
                }
                self.i += 1;
                let mut v: u32 = 0;
                let mut digits = 0;
                while let Some(d) = self.peek().and_then(|c| (c as char).to_digit(16)) {
                    v = v.checked_mul(16).and_then(|v| v.checked_add(d)).ok_or_else(|| self.err("\\u escape too large".into()))?;
                    digits += 1;
                    self.i += 1;
                }
                if digits == 0 || self.peek() != Some(b'}') || v > 0x7FFF_FFFF {
                    return Err(self.err("bad \\u escape".into()));
                }
                self.i += 1;
                let ch = char::from_u32(v).unwrap_or(char::REPLACEMENT_CHARACTER);
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
            b'0'..=b'9' => {
                let mut v: u32 = 0;
                for _ in 0..3 {
                    match self.peek() {
                        Some(d @ b'0'..=b'9') => {
                            v = v * 10 + (d - b'0') as u32;
                            self.i += 1;
                        }
                        _ => break,
                    }
                }
                if v > 255 {
                    return Err(self.err("decimal escape too large".into()));
                }
                out.push(v as u8);
            }
            _ => return Err(self.err(format!("invalid escape \\{}", c as char))),
        }
        Ok(())
    }

    fn table(&mut self) -> Result<Table, LuaError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.err(format!("tables nested deeper than {MAX_DEPTH}")));
        }
        self.i += 1; // '{'
        let mut t = Table::default();
        let mut next_pos: i64 = 1;
        loop {
            self.skip_space()?;
            match self.peek() {
                None => return Err(self.err("unfinished table".into())),
                Some(b'}') => {
                    self.i += 1;
                    break;
                }
                _ => {}
            }
            // `[key] = value`
            let key = if self.peek() == Some(b'[') && !matches!(self.peek_at(1), Some(b'[' | b'=')) {
                self.i += 1;
                self.skip_space()?;
                let k = self.value()?;
                self.skip_space()?;
                if self.peek() != Some(b']') {
                    return Err(self.err("expected ']'".into()));
                }
                self.i += 1;
                self.expect_assign()?;
                Some(self.key_of(k)?)
            } else if let Some(word) = self.named_field() {
                // `name = value`
                self.i += word.len();
                self.expect_assign()?;
                Some(Key::Str(String::from_utf8_lossy(&word).into_owned()))
            } else {
                None
            };
            self.skip_space()?;
            let v = self.value()?;
            let key = match key {
                Some(k) => k,
                None => {
                    let k = Key::Int(next_pos);
                    next_pos += 1;
                    k
                }
            };
            if v != Value::Nil {
                t.entries.push((key, v));
            }
            self.skip_space()?;
            match self.peek() {
                Some(b',' | b';') => self.i += 1,
                Some(b'}') => {}
                None => return Err(self.err("unfinished table".into())),
                Some(c) => return Err(self.err(format!("expected ',' or '}}', found {:?}", c as char))),
            }
        }
        self.depth -= 1;
        Ok(t)
    }

    /// If the cursor is at `name =` (not `name ==`), the name.
    fn named_field(&self) -> Option<Vec<u8>> {
        let word = self.peek_word()?.to_vec();
        let mut j = self.i + word.len();
        // Only white space may sit between the name and '='; comments there are rare
        // enough to refuse.
        while matches!(self.s.get(j), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            j += 1;
        }
        (self.s.get(j) == Some(&b'=') && self.s.get(j + 1) != Some(&b'=')).then_some(word)
    }

    fn expect_assign(&mut self) -> Result<(), LuaError> {
        self.skip_space()?;
        if self.peek() != Some(b'=') {
            return Err(self.err("expected '='".into()));
        }
        self.i += 1;
        Ok(())
    }

    fn key_of(&self, v: Value) -> Result<Key, LuaError> {
        Ok(match v {
            Value::Int(i) => Key::Int(i),
            Value::Num(n) if n.is_nan() => return Err(self.err("table key is NaN".into())),
            Value::Num(n) if n.fract() == 0.0 && n.abs() < 9.2e18 => Key::Int(n as i64),
            Value::Num(n) => Key::Num(n),
            Value::Str(s) => Key::Str(s),
            Value::Bool(b) => Key::Bool(b),
            Value::Nil => return Err(self.err("table key is nil".into())),
            Value::Table(_) => return Err(self.err("table keys must be literals".into())),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Value {
        parse(s.as_bytes()).unwrap_or_else(|e| panic!("{s:?}: {e}"))
    }

    fn t(s: &str) -> Table {
        parse_table(s.as_bytes()).unwrap_or_else(|e| panic!("{s:?}: {e}"))
    }

    #[test]
    fn scalars() {
        assert_eq!(p("return nil"), Value::Nil);
        assert_eq!(p("true"), Value::Bool(true));
        assert_eq!(p("return false;"), Value::Bool(false));
        assert_eq!(p("42"), Value::Int(42));
        assert_eq!(p("-7"), Value::Int(-7));
        assert_eq!(p("- 7"), Value::Int(-7));
        assert_eq!(p("0.5"), Value::Num(0.5));
        assert_eq!(p(".5"), Value::Num(0.5));
        assert_eq!(p("1e+300"), Value::Num(1e300));
        assert_eq!(p("1E-3"), Value::Num(1e-3));
        assert_eq!(p("9.007199254741e+15"), Value::Num(9.007199254741e15));
        assert_eq!(p("0x1F"), Value::Int(31));
        assert_eq!(p("0xffffffffffffffff"), Value::Int(-1));
        assert_eq!(p("0x1p4"), Value::Num(16.0));
        assert_eq!(p("0x.8"), Value::Num(0.5));
        assert_eq!(p("99999999999999999999"), Value::Num(1e20));
        assert_eq!(p("-9223372036854775808"), Value::Num(-9.223372036854776e18));
        assert_eq!(p("inf"), Value::Num(f64::INFINITY));
        assert_eq!(p("-inf"), Value::Num(f64::NEG_INFINITY));
        assert!(matches!(p("nan"), Value::Num(n) if n.is_nan()));
        assert!(matches!(p("-nan"), Value::Num(n) if n.is_nan()));
    }

    #[test]
    fn strings_and_escapes() {
        assert_eq!(p(r#""a\"b""#), Value::Str("a\"b".into()));
        assert_eq!(p(r"'it\'s'"), Value::Str("it's".into()));
        assert_eq!(p(r#""\a\b\f\n\r\t\v\\""#), Value::Str("\x07\x08\x0c\n\r\t\x0b\\".into()));
        assert_eq!(p("\"line1\\\nline2\""), Value::Str("line1\nline2".into()));
        assert_eq!(p("\"crlf\\\r\nx\""), Value::Str("crlf\nx".into()));
        assert_eq!(p(r#""\0001b""#), Value::Str("\u{0}1b".into()));
        assert_eq!(p(r#""\65\066\0677""#), Value::Str("ABC7".into()));
        assert_eq!(p(r#""\x41\x6a""#), Value::Str("Aj".into()));
        assert_eq!(p(r#""\u{48}\u{4E2D}\u{1F600}""#), Value::Str("H中😀".into()));
        assert_eq!(p("\"a\\z   \n  b\""), Value::Str("ab".into()));
        assert_eq!(p(r#""x\255y""#), Value::Str("x\u{FFFD}y".into()));
        assert_eq!(p("[[\nlong\r\nstring]]"), Value::Str("long\nstring".into()));
        assert_eq!(p("[==[a]]b]=]c]==]"), Value::Str("a]]b]=]c".into()));
        assert_eq!(p("\"\""), Value::Str(String::new()));
    }

    #[test]
    fn tables() {
        let v = t(r#"-- comment
            return {
                ["a"] = 1, b = "two"; [3] = true,
                "first", "second", nil, "fourth",
                [2.0] = "two-as-int", [2.5] = "f", [false] = 0,
                nested = { { x = 1 }, {} },
                --[==[ block
                comment ]==]
                ["gone"] = nil,
            }"#);
        assert_eq!(t("{}").entries.len(), 0);
        assert_eq!(v.int("a"), Some(1));
        assert_eq!(v.str("b"), Some("two"));
        assert_eq!(v.get_index(1), Some(&Value::Str("first".into())));
        // The positional "second" and [2.0] share key 2: the later one wins.
        assert_eq!(v.get_index(2), Some(&Value::Str("two-as-int".into())));
        // [3] = true and the positional nil: nil is dropped, true stays.
        assert_eq!(v.get_index(3), Some(&Value::Bool(true)));
        assert_eq!(v.get_index(4), Some(&Value::Str("fourth".into())));
        assert_eq!(v.seq().len(), 4);
        // Keys counted once: a, b, 1..=4, 2.5, false, nested.
        assert_eq!((v.entries.len(), v.len()), (10, 9));
        assert_eq!(t(r#"{ [1.5] = 1, [1.5] = 2, x = 1, ["x"] = 2, [true] = 1, [true] = 2 }"#).len(), 3);
        // Hand-built keys count as `==` compares them: -0.0 is 0.0, NaN equals nothing.
        let hand = Table { entries: [Key::Num(0.0), Key::Num(-0.0), Key::Num(f64::NAN), Key::Num(f64::NAN)].into_iter().map(|k| (k, Value::Nil)).collect() };
        assert_eq!(hand.len(), 3);
        assert!(v.get("gone").is_none());
        assert!(v.entries.iter().any(|(k, _)| *k == Key::Num(2.5)));
        assert!(v.entries.iter().any(|(k, _)| *k == Key::Bool(false)));
        let nested = v.table("nested").unwrap();
        assert_eq!(nested.seq()[0].as_table().unwrap().int("x"), Some(1));
        assert_eq!(Value::Table(v.clone()).to_json()["nested"][0]["x"], 1);
    }

    #[test]
    fn refuses_code() {
        for bad in [
            "os.execute('rm -rf /')",
            "return { a = os.getenv('HOME') }",
            "return { a = 1 + 2 }",
            "return { a = 'x' .. 'y' }",
            "return { a = function() end }",
            "return { a = b }",
            "return {} {}",
            "return { [{}] = 1 }",
            "return { [nil] = 1 }",
            "return { [0/0] = 1 }",
            "return { a = 1",
            "return 'unfinished",
            "return \"a\nb\"",
            "return [[never closed",
            "return 12abc",
            "return 0x",
            "return \"\\q\"",
            "return \"\\256\"",
            "return \"\\u{110000000}\"",
            "",
            "--[[ unclosed comment",
        ] {
            assert!(parse(bad.as_bytes()).is_err(), "accepted {bad:?}");
        }
        assert!(parse_table(b"return 1").is_err());
    }

    #[test]
    fn value_limit() {
        // The outer table is one value, each `1` another: MAX_VALUES in all is accepted.
        let mut src = b"return {".to_vec();
        src.extend(b"1,".repeat(MAX_VALUES - 1));
        src.push(b'}');
        assert!(src.len() <= MAX_INPUT);
        assert_eq!(parse_table(&src).unwrap().entries.len(), MAX_VALUES - 1);
        // One more is refused with an error, keys and nil count too.
        src.truncate(src.len() - 1);
        src.extend(b"nil}");
        assert!(parse(&src).unwrap_err().msg.contains("values"));
        assert!(parse(b"return {[1]=1,[2]=2}").is_ok());
    }

    #[test]
    fn depth_limit_and_garbage_never_panic() {
        let deep = "{".repeat(MAX_DEPTH + 1) + &"}".repeat(MAX_DEPTH + 1);
        assert!(parse(deep.as_bytes()).is_err());
        let ok = "{".repeat(MAX_DEPTH) + &"}".repeat(MAX_DEPTH);
        assert!(parse(ok.as_bytes()).is_ok());
        // Pseudo-random bytes and truncations of a valid file: errors, no panics.
        let src = include_bytes!("../tests/fixtures/nasty.lua");
        for cut in 0..src.len() {
            let _ = parse(&src[..cut]);
        }
        let alphabet = b"{}[]=,;\"'\\-0123456789.exnil truefalse\n\r";
        let mut x: u32 = 1;
        for _ in 0..5000 {
            let mut buf = Vec::new();
            for _ in 0..64 {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                buf.push(alphabet[x as usize % alphabet.len()]);
            }
            let _ = parse(&buf);
        }
        let minus = "- ".repeat(100_000) + "1";
        assert!(parse(minus.as_bytes()).unwrap_err().msg.contains("nested operators"));
        assert_eq!(parse(b"- - 1").unwrap(), Value::Int(1));
    }

    #[test]
    fn koreader_dump_fixture() {
        // Written by KOReader's frontend/dump.lua under LuaJIT (tests/fixtures/gen-dump.lua).
        let v = parse_table(include_bytes!("../tests/fixtures/nasty.lua")).unwrap();
        assert_eq!(v.str("quote"), Some("say \"hi\""));
        assert_eq!(v.str("backslash"), Some("a\\b\\\\c"));
        assert_eq!(v.str("newline"), Some("line1\nline2\n"));
        assert_eq!(v.str("cr"), Some("a\rb"));
        assert_eq!(v.str("nul"), Some("a\0b"));
        assert_eq!(v.str("nul_digit"), Some("a\u{0}1b"));
        assert_eq!(v.str("controls"), Some("\x01\x02\x07\x08\x09\x0b\x0c\x1b\x1f\x7f"));
        assert_eq!(v.str("cjk"), Some("中文 text"));
        assert_eq!(v.str("emoji"), Some("😀"));
        assert_eq!(v.str("bad_utf8"), Some("x\u{FFFD}y"));
        assert_eq!(v.str("empty"), Some(""));
        assert_eq!(v.get("int"), Some(&Value::Int(42)));
        assert_eq!(v.get("neg"), Some(&Value::Int(-7)));
        assert_eq!(v.get("half"), Some(&Value::Num(0.5)));
        assert_eq!(v.num("third"), Some(0.33333333333333));
        assert_eq!(v.num("big"), Some(1e300));
        assert_eq!(v.num("tiny"), Some(1e-300));
        assert_eq!(v.num("two53"), Some(9.007199254741e15));
        assert_eq!(v.num("huge"), Some(f64::INFINITY));
        assert_eq!(v.num("neghuge"), Some(f64::NEG_INFINITY));
        assert_eq!(v.bool("yes"), Some(true));
        assert_eq!(v.bool("no"), Some(false));
        let list = v.table("list").unwrap().seq();
        assert_eq!(list.len(), 5);
        assert_eq!(list[3].as_str(), Some("x"));
        let holes = v.table("holes").unwrap();
        assert_eq!(holes.seq().len(), 1);
        assert_eq!(holes.int_entries().iter().map(|(k, _)| *k).collect::<Vec<_>>(), [1, 3]);
        let numkeys = v.table("numkeys").unwrap();
        assert_eq!(numkeys.get_index(-1).and_then(Value::as_str), Some("m"));
        assert_eq!(numkeys.get_index(0).and_then(Value::as_str), Some("z"));
        assert_eq!(v.table("nested").unwrap().table("a").unwrap().table("b").unwrap().table("c").unwrap().str("d"), Some("deep"));
        assert!(v.table("empty_table").unwrap().is_empty());
        // dump.lua writes a cycle as `nil --[[ LOOP: ... ]]`.
        let lp = parse_table(include_bytes!("../tests/fixtures/loop.lua")).unwrap();
        assert_eq!(lp.str("name"), Some("loop"));
        assert!(lp.get("self").is_none());
    }
}
