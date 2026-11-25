//! A small parser for Valve's text KeyValues format (`libraryfolders.vdf`, `appmanifest_*.acf`,
//! `localconfig.vdf`). Quoted strings with `\n \t \\ \"` escapes, `//` comments, nested `{}` sections.
//!
//! Keys are matched case-insensitively (Steam is inconsistent: `AppState`, `appid`, `LastPlayed`).

#[derive(Debug, Clone, PartialEq)]
pub enum Vdf {
    Str(String),
    Obj(Vec<(String, Vdf)>),
}

impl Vdf {
    /// Parse a document. The result is always an object holding the top-level key(s).
    /// Malformed input yields what was parsed up to the error (Steam rewrites these files while running).
    pub fn parse(text: &str) -> Vdf {
        let mut p = Parser { s: text.as_bytes(), i: 0 };
        Vdf::Obj(p.object(0))
    }

    pub fn get(&self, key: &str) -> Option<&Vdf> {
        match self {
            Vdf::Obj(items) => items.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v),
            Vdf::Str(_) => None,
        }
    }

    pub fn path(&self, keys: &[&str]) -> Option<&Vdf> {
        keys.iter().try_fold(self, |cur, k| cur.get(k))
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Vdf::Str(s) => Some(s),
            Vdf::Obj(_) => None,
        }
    }

    pub fn str_at(&self, keys: &[&str]) -> Option<&str> {
        self.path(keys).and_then(Vdf::as_str)
    }

    pub fn u64_at(&self, keys: &[&str]) -> Option<u64> {
        self.str_at(keys).and_then(|s| s.trim().parse().ok())
    }

    pub fn entries(&self) -> &[(String, Vdf)] {
        match self {
            Vdf::Obj(items) => items,
            Vdf::Str(_) => &[],
        }
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

const MAX_DEPTH: usize = 64;

impl Parser<'_> {
    fn skip_ws(&mut self) {
        loop {
            while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
            if self.s[self.i..].starts_with(b"//") {
                while self.i < self.s.len() && self.s[self.i] != b'\n' {
                    self.i += 1;
                }
            } else {
                break;
            }
        }
    }

    /// Read items until `}` or end of input.
    fn object(&mut self, depth: usize) -> Vec<(String, Vdf)> {
        let mut items = Vec::new();
        if depth > MAX_DEPTH {
            return items;
        }
        loop {
            self.skip_ws();
            let Some(&c) = self.s.get(self.i) else { break };
            if c == b'}' {
                self.i += 1;
                break;
            }
            if c == b'{' {
                // Stray brace without a key: skip it.
                self.i += 1;
                continue;
            }
            let key = self.token();
            self.skip_ws();
            match self.s.get(self.i) {
                Some(b'{') => {
                    self.i += 1;
                    items.push((key, Vdf::Obj(self.object(depth + 1))));
                }
                Some(b'}') | None => break,
                Some(_) => {
                    let value = self.token();
                    items.push((key, Vdf::Str(value)));
                }
            }
        }
        items
    }

    fn token(&mut self) -> String {
        if self.s.get(self.i) == Some(&b'"') {
            self.i += 1;
            let mut out = Vec::new();
            while let Some(&c) = self.s.get(self.i) {
                self.i += 1;
                match c {
                    b'"' => break,
                    b'\\' => match self.s.get(self.i) {
                        Some(b'n') => {
                            out.push(b'\n');
                            self.i += 1;
                        }
                        Some(b't') => {
                            out.push(b'\t');
                            self.i += 1;
                        }
                        Some(&e @ (b'\\' | b'"')) => {
                            out.push(e);
                            self.i += 1;
                        }
                        // Unknown escape: keep the backslash (Windows paths in old files).
                        _ => out.push(b'\\'),
                    },
                    _ => out.push(c),
                }
            }
            String::from_utf8_lossy(&out).into_owned()
        } else {
            let start = self.i;
            while self.i < self.s.len() && !self.s[self.i].is_ascii_whitespace() && !matches!(self.s[self.i], b'{' | b'}' | b'"') {
                self.i += 1;
            }
            String::from_utf8_lossy(&self.s[start..self.i]).into_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_and_escapes() {
        let v = Vdf::parse(
            "// comment\n\"Root\"\n{\n\t\"a\"\t\t\"1\"\n\t\"Sub\"\n\t{\n\t\t\"q\"\t\"say \\\"hi\\\"\\n\"\n\t\t\"z\" { \"deep\" \"x\" }\n\t}\n\tbare value\n}\n",
        );
        assert_eq!(v.str_at(&["root", "A"]), Some("1"));
        assert_eq!(v.str_at(&["Root", "Sub", "q"]), Some("say \"hi\"\n"));
        assert_eq!(v.str_at(&["Root", "Sub", "z", "deep"]), Some("x"));
        assert_eq!(v.str_at(&["Root", "bare"]), Some("value"));
        assert_eq!(v.u64_at(&["Root", "a"]), Some(1));
        assert_eq!(v.path(&["Root", "missing"]), None);
    }

    #[test]
    fn truncated_input_does_not_panic() {
        for text in ["\"a\" {", "\"a\" { \"b\"", "\"a\" { \"b\" \"c", "}}}", "{", "\"", ""] {
            let _ = Vdf::parse(text);
        }
        let v = Vdf::parse("\"a\" { \"b\" \"c\" ");
        assert_eq!(v.str_at(&["a", "b"]), Some("c"));
    }

    #[test]
    fn deep_nesting_is_bounded() {
        let text = "\"k\" {".repeat(10_000);
        let _ = Vdf::parse(&text);
    }
}
