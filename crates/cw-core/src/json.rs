//! Minimal JSON reader for machine-generated artifacts (oracle.json now,
//! CW `CaseWareProtectionInfo`-family payloads later). Zero dependencies.
//!
//! Not a general-purpose JSON implementation: no number formatting beyond
//! f64/i64 capture, objects preserve insertion order (Vec-based map).

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_usize(&self) -> Option<usize> {
        self.as_f64().map(|f| f as usize)
    }

    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_obj(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Obj(o) => Some(o),
            _ => None,
        }
    }
}

pub fn parse(input: &str) -> Result<Json, String> {
    let bytes = input.as_bytes();
    let mut p = Parser { b: bytes, i: 0 };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != p.b.len() {
        return Err(format!("trailing data at {}", p.i));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.i += 1;
        }
    }

    fn expect(&mut self, c: u8) -> Result<(), String> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected '{c}' at {}", self.i))
        }
    }

    fn literal(&mut self, s: &str, v: Json) -> Result<Json, String> {
        let sb = s.as_bytes();
        if self.b.len() >= self.i + sb.len() && &self.b[self.i..self.i + sb.len()] == sb {
            self.i += sb.len();
            Ok(v)
        } else {
            Err(format!("expected {s} at {}", self.i))
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            other => Err(format!("unexpected {other:?} at {}", self.i)),
        }
    }

    fn object(&mut self) -> Result<Json, String> {
        self.expect(b'{')?;
        let mut pairs = Vec::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Json::Obj(pairs));
        }
        loop {
            self.ws();
            let key = self.string()?;
            self.ws();
            self.expect(b':')?;
            self.ws();
            let val = self.value()?;
            pairs.push((key, val));
            self.ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Json::Obj(pairs));
                }
                other => return Err(format!("expected ',' or '}}', got {other:?} at {}", self.i)),
            }
        }
    }

    fn array(&mut self) -> Result<Json, String> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            self.ws();
            items.push(self.value()?);
            self.ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Json::Arr(items));
                }
                other => return Err(format!("expected ',' or ']', got {other:?} at {}", self.i)),
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop
        {
            let Some(c) = self.peek() else {
                return Err("unterminated string".into());
            };
            self.i += 1;
            match c {
                b'"' => return Ok(out),
                b'\\' => {
                    let Some(e) = self.peek() else {
                        return Err("bad escape".into());
                    };
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hex = self
                                .b
                                .get(self.i..self.i + 4)
                                .ok_or("bad \\u escape")?;
                            let s = std::str::from_utf8(hex).map_err(|_| "bad \\u hex")?;
                            let cp = u32::from_str_radix(s, 16).map_err(|_| "bad \\u hex")?;
                            self.i += 4;
                            // Astral pairs written as surrogate escapes are not
                            // produced by our emitters; BMP only.
                            if (0xD800..0xDC00).contains(&cp) {
                                // low surrogate follows
                                if self.b.get(self.i) == Some(&b'\\')
                                    && self.b.get(self.i + 1) == Some(&b'u')
                                {
                                    let hex2 = self
                                        .b
                                        .get(self.i + 2..self.i + 6)
                                        .ok_or("bad surrogate pair")?;
                                    let s2 = std::str::from_utf8(hex2).map_err(|_| "bad pair")?;
                                    let lo = u32::from_str_radix(s2, 16).map_err(|_| "bad pair")?;
                                    self.i += 6;
                                    let c = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                    out.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
                                } else {
                                    out.push('\u{FFFD}');
                                }
                            } else {
                                out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                            }
                        }
                        other => return Err(format!("bad escape \\{other}")),
                    }
                }
                _ => {
                    // copy the raw UTF-8 sequence: back up and take chars
                    let start = self.i - 1;
                    // find a char boundary forward
                    let mut end = self.i;
                    while end < self.b.len() && (self.b[end] & 0xC0) == 0x80 {
                        end += 1;
                    }
                    self.i = end;
                    let slice = std::str::from_utf8(&self.b[start..end])
                        .map_err(|_| "invalid utf8 in string")?;
                    let ch = slice.chars().next().ok_or("empty char")?;
                    out.push(ch);
                }
            }
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-'))
        {
            self.i += 1;
        }
        let s = std::str::from_utf8(&self.b[start..self.i]).map_err(|_| "bad number")?;
        s.parse::<f64>()
            .map(Json::Num)
            .map_err(|e| format!("bad number '{s}': {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_basics() {
        let v = parse(r#"{"a":1,"b":[true,null,"x\ny"],"c":{"d":-2.5}}"#).unwrap();
        assert_eq!(v.get("a").and_then(|x| x.as_f64()), Some(1.0));
        let b = v.get("b").and_then(|x| x.as_arr()).unwrap();
        assert_eq!(b.len(), 3);
        assert_eq!(b[2].as_str(), Some("x\ny"));
        assert_eq!(
            v.get("c").unwrap().get("d").and_then(|x| x.as_f64()),
            Some(-2.5)
        );
    }

    #[test]
    fn unicode_escape() {
        let v = parse(r#""e\u00e9""#).unwrap();
        assert_eq!(v.as_str(), Some("e\u{e9}"));
    }
}

