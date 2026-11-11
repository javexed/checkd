//! The values of columns, and how an answer writes them.

use std::fmt::Write;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    /// A time with its fraction, as `last_update` is written:
    /// `1794424316.342238`.
    Time(f64),
    Str(String),
    List(Vec<Value>),
    /// A dictionary of custom variables: name to value.
    Dict(Vec<(String, String)>),
}

impl Value {
    pub fn str(text: impl Into<String>) -> Value {
        Value::Str(text.into())
    }

    pub fn bool(on: bool) -> Value {
        Value::Int(on as i64)
    }

    pub fn strs<I: IntoIterator<Item = S>, S: Into<String>>(items: I) -> Value {
        Value::List(items.into_iter().map(|s| Value::Str(s.into())).collect())
    }

    /// The value as a number, for sorting, stats and filters.
    pub fn number(&self) -> f64 {
        match self {
            Value::Int(n) => *n as f64,
            Value::Float(x) | Value::Time(x) => *x,
            Value::Str(s) => s.trim().parse().unwrap_or(0.0),
            Value::List(items) => items.len() as f64,
            Value::Dict(items) => items.len() as f64,
        }
    }

    /// The value as text, for filters and CSV.
    pub fn text(&self) -> String {
        match self {
            Value::Int(n) => n.to_string(),
            Value::Float(x) => float(*x),
            Value::Time(x) => format!("{x:.6}"),
            Value::Str(s) => s.clone(),
            Value::List(items) => items.iter().map(Value::text).collect::<Vec<_>>().join(","),
            Value::Dict(items) => items
                .iter()
                .map(|(k, v)| format!("{k}|{v}"))
                .collect::<Vec<_>>()
                .join(","),
        }
    }

    pub fn write_json(&self, out: &mut String) {
        match self {
            Value::Int(n) => {
                let _ = write!(out, "{n}");
            }
            Value::Float(x) => out.push_str(&float(*x)),
            Value::Time(x) => {
                let _ = write!(out, "{x:.6}");
            }
            Value::Str(s) => json_string(s, out),
            Value::List(items) => {
                out.push('[');
                for (n, item) in items.iter().enumerate() {
                    if n > 0 {
                        out.push(',');
                    }
                    item.write_json(out);
                }
                out.push(']');
            }
            Value::Dict(items) => {
                out.push('{');
                for (n, (key, value)) in items.iter().enumerate() {
                    if n > 0 {
                        out.push(',');
                    }
                    json_string(key, out);
                    out.push(':');
                    json_string(value, out);
                }
                out.push('}');
            }
        }
    }
}

/// A float as the reference writes one: ten digits after the point and
/// an exponent of at least two digits, `1.0000000000e+00`.
pub fn float(x: f64) -> String {
    if !x.is_finite() {
        return if x.is_nan() { "nan".into() } else if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let text = format!("{x:.10e}");
    let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let sign = if exponent < 0 { '-' } else { '+' };
    format!("{mantissa}e{sign}{:02}", exponent.abs())
}

pub fn json_string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_as_the_reference() {
        assert_eq!(float(1.0), "1.0000000000e+00");
        assert_eq!(float(0.004179), "4.1790000000e-03");
        assert_eq!(float(0.0), "0.0000000000e+00");
        assert_eq!(float(30.0), "3.0000000000e+01");
        assert_eq!(float(8.421052631578947), "8.4210526316e+00");
    }

    #[test]
    fn json() {
        let mut out = String::new();
        Value::List(vec![Value::Int(2), Value::str("a\"b"), Value::Time(1794424316.342238)]).write_json(&mut out);
        assert_eq!(out, "[2,\"a\\\"b\",1794424316.342238]");
    }
}
