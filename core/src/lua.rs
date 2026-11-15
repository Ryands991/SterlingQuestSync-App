//! A strict reader for WoW SavedVariables files. It parses data only and never executes anything.
//!
//! Supports what the client writes: `Name = value` assignments, tables with `["key"] =` / `[n] =`
//! keys or plain list items, strings, numbers, booleans and nil, plus `--` comments.

use std::collections::BTreeMap;

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Nil,
    Bool(bool),
    Num(f64),
    Str(String),
    Table(Table),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Table {
    pub map: BTreeMap<String, Value>,
    pub list: Vec<Value>,
}

impl Value {
    pub fn as_table(&self) -> Option<&Table> {
        match self {
            Value::Table(t) => Some(t),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Num(n) if n.fract() == 0.0 && n.abs() < 9.0e15 => Some(*n as i64),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

impl Table {
    pub fn get(&self, k: &str) -> Option<&Value> {
        self.map.get(k)
    }
}

const MAX_DEPTH: usize = 32;
const MAX_INPUT: usize = 32 * 1024 * 1024;

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

fn err<T>(msg: &str, at: usize) -> Result<T> {
    Err(Error::Parse(format!("{} at byte {}", msg, at)))
}

impl<'a> P<'a> {
    fn skip(&mut self) {
        loop {
            while self.i < self.s.len() && (self.s[self.i] as char).is_ascii_whitespace() {
                self.i += 1;
            }
            if self.s[self.i..].starts_with(b"--") {
                // Block comments --[[ ]] are not written by the client; treat any comment as line comment.
                while self.i < self.s.len() && self.s[self.i] != b'\n' {
                    self.i += 1;
                }
                continue;
            }
            break;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.skip();
        if self.i < self.s.len() && self.s[self.i] == c {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn ident(&mut self) -> Option<String> {
        self.skip();
        let start = self.i;
        while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_') {
            self.i += 1;
        }
        if self.i == start || self.s[start].is_ascii_digit() {
            self.i = start;
            return None;
        }
        Some(String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }

    fn string(&mut self) -> Result<String> {
        let q = self.s[self.i];
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        while self.i < self.s.len() {
            let c = self.s[self.i];
            self.i += 1;
            if c == q {
                return Ok(String::from_utf8_lossy(&out).into_owned());
            }
            if c == b'\n' {
                return err("newline in string", self.i);
            }
            if c != b'\\' {
                out.push(c);
                continue;
            }
            if self.i >= self.s.len() {
                break;
            }
            let e = self.s[self.i];
            self.i += 1;
            match e {
                b'n' => out.push(b'\n'),
                b't' => out.push(b'\t'),
                b'r' => out.push(b'\r'),
                b'\\' => out.push(b'\\'),
                b'"' => out.push(b'"'),
                b'\'' => out.push(b'\''),
                b'\n' => out.push(b'\n'),
                b'0'..=b'9' => {
                    let mut n: u32 = (e - b'0') as u32;
                    for _ in 0..2 {
                        if self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                            n = n * 10 + (self.s[self.i] - b'0') as u32;
                            self.i += 1;
                        }
                    }
                    if n > 255 {
                        return err("bad escape", self.i);
                    }
                    out.push(n as u8);
                }
                _ => return err("unknown escape", self.i),
            }
        }
        err("unterminated string", self.i)
    }

    fn number(&mut self) -> Result<f64> {
        let start = self.i;
        while self.i < self.s.len() && matches!(self.s[self.i], b'0'..=b'9' | b'.' | b'-' | b'+' | b'e' | b'E' | b'x' | b'X' | b'a'..=b'f' | b'A'..=b'F') {
            self.i += 1;
        }
        let t = std::str::from_utf8(&self.s[start..self.i]).unwrap_or("");
        if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
            return i64::from_str_radix(h, 16).map(|v| v as f64).or_else(|_| err("bad hex number", start));
        }
        if let Some(h) = t.strip_prefix("-0x") {
            return i64::from_str_radix(h, 16).map(|v| -(v as f64)).or_else(|_| err("bad hex number", start));
        }
        match t.parse::<f64>() {
            Ok(v) if v.is_finite() => Ok(v),
            _ => err("bad number", start),
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value> {
        if depth > MAX_DEPTH {
            return err("nested too deep", self.i);
        }
        self.skip();
        if self.i >= self.s.len() {
            return err("unexpected end", self.i);
        }
        match self.s[self.i] {
            b'{' => {
                self.i += 1;
                self.table(depth + 1).map(Value::Table)
            }
            b'"' | b'\'' => self.string().map(Value::Str),
            b'-' | b'0'..=b'9' | b'.' => self.number().map(Value::Num),
            _ => match self.ident().as_deref() {
                Some("true") => Ok(Value::Bool(true)),
                Some("false") => Ok(Value::Bool(false)),
                Some("nil") => Ok(Value::Nil),
                Some(other) if other == "inf" || other == "nan" => err("non-finite number", self.i),
                _ => err("unexpected token", self.i),
            },
        }
    }

    fn table(&mut self, depth: usize) -> Result<Table> {
        let mut t = Table::default();
        loop {
            if self.eat(b'}') {
                return Ok(t);
            }
            self.skip();
            if self.i < self.s.len() && self.s[self.i] == b'[' {
                self.i += 1;
                self.skip();
                let k = match self.value(depth)? {
                    Value::Str(s) => s,
                    Value::Num(n) if n.fract() == 0.0 => format!("{}", n as i64),
                    Value::Bool(b) => b.to_string(),
                    _ => return err("bad table key", self.i),
                };
                if !self.eat(b']') || !self.eat(b'=') {
                    return err("expected ] =", self.i);
                }
                let v = self.value(depth)?;
                t.map.insert(k, v);
            } else {
                let save = self.i;
                if let Some(id) = self.ident() {
                    if self.eat(b'=') {
                        let v = self.value(depth)?;
                        t.map.insert(id, v);
                    } else {
                        self.i = save;
                        let v = self.value(depth)?;
                        t.list.push(v);
                    }
                } else {
                    let v = self.value(depth)?;
                    t.list.push(v);
                }
            }
            if !self.eat(b',') && !self.eat(b';') {
                if self.eat(b'}') {
                    return Ok(t);
                }
                return err("expected , or }", self.i);
            }
        }
    }
}

/// Parses a whole SavedVariables file into its top-level assignments.
pub fn parse_saved_variables(text: &str) -> Result<BTreeMap<String, Value>> {
    if text.len() > MAX_INPUT {
        return Err(Error::Parse("file too large".into()));
    }
    let mut p = P { s: text.as_bytes(), i: 0 };
    // Skip a UTF-8 byte order mark.
    if p.s.starts_with(&[0xEF, 0xBB, 0xBF]) {
        p.i = 3;
    }
    let mut out = BTreeMap::new();
    loop {
        p.skip();
        if p.i >= p.s.len() {
            return Ok(out);
        }
        let name = p.ident().ok_or_else(|| Error::Parse(format!("expected a name at byte {}", p.i)))?;
        if !p.eat(b'=') {
            return err("expected =", p.i);
        }
        let v = p.value(0)?;
        out.insert(name, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_what_the_client_writes() {
        let text = r#"
SterlingQuestSyncOut = {
	["format"] = 1,
	["characters"] = {
		["Player-6800-0000A11C"] = {
			["name"] = "Kaelith Emberfall",
			["saved"] = 1794770000,
			["ratio"] = 0.5,
			["ok"] = true,
			["note"] = "quote \" back \\ nl \n tab \t byte \065",
			["list"] = {
				"a", -- [1]
				"b", -- [2]
			},
		},
	},
}
OtherVar = nil
Third = -12
"#;
        let m = parse_saved_variables(text).unwrap();
        let out = m["SterlingQuestSyncOut"].as_table().unwrap();
        assert_eq!(out.get("format").unwrap().as_i64(), Some(1));
        let c = out.get("characters").unwrap().as_table().unwrap().get("Player-6800-0000A11C").unwrap().as_table().unwrap();
        assert_eq!(c.get("name").unwrap().as_str(), Some("Kaelith Emberfall"));
        assert_eq!(c.get("saved").unwrap().as_i64(), Some(1794770000));
        assert_eq!(c.get("ok").unwrap().as_bool(), Some(true));
        assert_eq!(c.get("note").unwrap().as_str(), Some("quote \" back \\ nl \n tab \t byte A"));
        assert_eq!(c.get("list").unwrap().as_table().unwrap().list.len(), 2);
        assert_eq!(m["OtherVar"], Value::Nil);
        assert_eq!(m["Third"].as_i64(), Some(-12));
    }

    #[test]
    fn refuses_code() {
        for bad in [
            "X = os.exit()",
            "X = { a = function() end }",
            "X = loadstring('x')",
            "X = { [\"a\"] = 1 } os.execute('rm')",
            "X = \"unterminated",
            "X = {",
            "X = 1e999999 + 1",
        ] {
            assert!(parse_saved_variables(bad).is_err(), "{}", bad);
        }
    }

    #[test]
    fn refuses_deep_nesting() {
        let deep = format!("X = {}{}", "{".repeat(100), "}".repeat(100));
        assert!(parse_saved_variables(&deep).is_err());
    }
}
