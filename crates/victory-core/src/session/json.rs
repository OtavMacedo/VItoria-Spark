//! Just enough JSON writing for the session files (no parser, no extra
//! dependency): strings escaped, numbers that are not finite as `null`.

use std::fmt::Write;

/// Appends `s` as a JSON string (with its quotes).
pub(crate) fn string_into(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `s` as a JSON string.
pub(crate) fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    string_into(&mut out, s);
    out
}

/// A number: integers without a fraction, others with at most three
/// decimals; NaN and infinities as `null`.
pub(crate) fn number(v: f64) -> String {
    if !v.is_finite() {
        "null".to_owned()
    } else if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{v:.0}")
    } else {
        let s = format!("{v:.3}");
        let s = s.trim_end_matches('0').trim_end_matches('.');
        s.to_owned()
    }
}

/// An array of values already written as JSON.
pub(crate) fn array(items: impl IntoIterator<Item = String>) -> String {
    let mut out = String::from("[");
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&item);
    }
    out.push(']');
    out
}

/// An object written key by key, in order.
pub(crate) struct Obj {
    out: String,
    empty: bool,
}

impl Obj {
    pub(crate) fn new() -> Self {
        Obj {
            out: String::from("{"),
            empty: true,
        }
    }

    fn key(&mut self, k: &str) {
        if !self.empty {
            self.out.push(',');
        }
        self.empty = false;
        string_into(&mut self.out, k);
        self.out.push(':');
    }

    pub(crate) fn str(mut self, k: &str, v: &str) -> Self {
        self.key(k);
        string_into(&mut self.out, v);
        self
    }

    pub(crate) fn num(mut self, k: &str, v: f64) -> Self {
        self.key(k);
        self.out.push_str(&number(v));
        self
    }

    pub(crate) fn int(mut self, k: &str, v: u64) -> Self {
        self.key(k);
        let _ = write!(self.out, "{v}");
        self
    }

    pub(crate) fn opt_num(self, k: &str, v: Option<f64>) -> Self {
        match v {
            Some(v) => self.num(k, v),
            None => self.raw(k, "null"),
        }
    }

    pub(crate) fn bool(mut self, k: &str, v: bool) -> Self {
        self.key(k);
        self.out.push_str(if v { "true" } else { "false" });
        self
    }

    /// A value already written as JSON.
    pub(crate) fn raw(mut self, k: &str, json: &str) -> Self {
        self.key(k);
        self.out.push_str(json);
        self
    }

    /// Adds in place (for loops).
    pub(crate) fn push_raw(&mut self, k: &str, json: &str) {
        self.key(k);
        self.out.push_str(json);
    }

    pub(crate) fn finish(mut self) -> String {
        self.out.push('}');
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_are_escaped() {
        assert_eq!(string("a\"b\\c\nd\u{1}"), r#""a\"b\\c\nd\u0001""#);
    }

    #[test]
    fn numbers() {
        assert_eq!(number(3.0), "3");
        assert_eq!(number(2.5), "2.5");
        assert_eq!(number(1.23456), "1.235");
        assert_eq!(number(f64::NAN), "null");
    }

    #[test]
    fn objects() {
        let o = Obj::new()
            .str("a", "x")
            .num("b", 1.5)
            .int("c", 7)
            .bool("d", true)
            .opt_num("e", None)
            .raw("f", &array(["1".into(), "2".into()]))
            .finish();
        assert_eq!(o, r#"{"a":"x","b":1.5,"c":7,"d":true,"e":null,"f":[1,2]}"#);
    }
}
