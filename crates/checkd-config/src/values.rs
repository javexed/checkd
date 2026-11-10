//! Parsing of setting values.

use crate::schema::Kind;

/// A set of option letters, such as the states to notify on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opts {
    /// The letters that can be set, in the order they are written out.
    allowed: &'static str,
    /// The letters that are set, in the same order.
    letters: String,
    /// Set by `a`, and by a default of "all". Naming every letter is not
    /// the same thing: the reference dump keeps the two apart, so this
    /// does too.
    all: bool,
}

impl Opts {
    pub fn all(allowed: &'static str) -> Opts {
        Opts {
            allowed,
            letters: allowed.to_string(),
            all: true,
        }
    }

    pub fn none(allowed: &'static str) -> Opts {
        Opts {
            allowed,
            letters: String::new(),
            all: false,
        }
    }

    pub fn parse(value: &str, allowed: &'static str) -> Result<Opts, String> {
        let mut opts = Opts::none(allowed);
        let mut chosen = Vec::new();
        for item in value
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            match item {
                "a" => opts.all = true,
                "n" => {
                    opts.all = false;
                    chosen.clear();
                }
                _ if item.len() == 1 && allowed.contains(item) => chosen.push(item),
                _ => {
                    let list = allowed
                        .chars()
                        .map(String::from)
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(format!(
                        "'{item}' is not an option here (the options are {list}, a for all, n for none)"
                    ));
                }
            }
        }
        opts.letters = if opts.all {
            allowed.to_string()
        } else {
            allowed
                .chars()
                .filter(|c| chosen.iter().any(|item| item.starts_with(*c)))
                .collect()
        };
        Ok(opts)
    }

    pub fn has(&self, letter: char) -> bool {
        self.letters.contains(letter)
    }

    pub fn is_none(&self) -> bool {
        self.letters.is_empty()
    }

    /// The letters that are set, in the order they are written out.
    pub fn letters(&self) -> &str {
        &self.letters
    }

    /// As written in the resolved-object dump.
    pub fn dump(&self) -> String {
        if self.all {
            "a".to_string()
        } else if self.letters.is_empty() {
            "n".to_string()
        } else {
            self.letters
                .chars()
                .map(String::from)
                .collect::<Vec<_>>()
                .join(",")
        }
    }
}

pub fn parse_flag(value: &str) -> Result<bool, String> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(format!("'{value}' is not 0 or 1")),
    }
}

/// A whole number, read strictly: digits, with an optional `+` in front.
pub fn parse_int_strict(value: &str) -> Result<u32, String> {
    match read_int(value) {
        Ok((number, true)) => Ok(number),
        Ok(_) => Err(format!("'{value}' is not a whole number of zero or more")),
        Err(why) => Err(why),
    }
}

/// The largest whole number taken. The reference wraps round above this.
const INT_MAX: u64 = i32::MAX as u64;

/// Reads a whole number the way the reference does: the digits at the
/// start count, and whatever follows them is dropped. No digits at the
/// start is 0. The flag says whether the value was a plain number.
///
/// Stricter than the reference in two ways: a number below zero and a
/// number too large to hold are errors.
pub fn read_int(value: &str) -> Result<(u32, bool), String> {
    let (negative, rest) = match value.as_bytes().first() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    };
    let length = rest.bytes().take_while(u8::is_ascii_digit).count();
    let digits = rest[..length].trim_start_matches('0');
    let number: u64 = match digits.len() {
        0 => 0,
        1..=10 => digits.parse().unwrap_or(u64::MAX),
        _ => u64::MAX,
    };
    if number > INT_MAX {
        return Err(format!("'{value}' is too large"));
    }
    if negative && number > 0 {
        return Err(format!("'{value}' is below zero"));
    }
    Ok((number as u32, length > 0 && length == rest.len()))
}

pub fn parse_int(value: &str) -> Result<u32, String> {
    read_int(value).map(|(number, _)| number)
}

/// Reads a number the way the reference does: the number at the start
/// counts, and whatever follows it is dropped. No number at the start is 0.
/// The flag says whether the value was a plain number.
///
/// Stricter than the reference in three ways: a number below zero, a
/// number too large to hold, and the forms `0x...`, `inf` and `nan` are
/// errors.
pub fn read_float(value: &str) -> Result<(f64, bool), String> {
    let bytes = value.as_bytes();
    let mut at = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let unsigned = value[at..].to_ascii_lowercase();
    if ["0x", "inf", "nan"]
        .iter()
        .any(|form| unsigned.starts_with(form))
    {
        return Err(format!("'{value}' is not a number written in digits"));
    }
    let digits = |from: usize| {
        bytes[from..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count()
    };
    let whole = digits(at);
    at += whole;
    let mut fraction = 0;
    if bytes.get(at) == Some(&b'.') {
        fraction = digits(at + 1);
        if whole + fraction > 0 {
            at += 1 + fraction;
        }
    }
    if whole + fraction == 0 {
        return Ok((0.0, false));
    }
    if matches!(bytes.get(at), Some(b'e' | b'E')) {
        let sign = usize::from(matches!(bytes.get(at + 1), Some(b'+' | b'-')));
        let exponent = digits(at + 1 + sign);
        if exponent > 0 {
            at += 1 + sign + exponent;
        }
    }
    let number: f64 = value[..at]
        .parse()
        .map_err(|_| format!("'{value}' is not a number"))?;
    if !number.is_finite() {
        return Err(format!("'{value}' is too large"));
    }
    if number < 0.0 {
        return Err(format!("'{value}' is below zero"));
    }
    Ok((number, at == value.len()))
}

pub fn parse_float(value: &str) -> Result<f64, String> {
    read_float(value).map(|(number, _)| number)
}

pub fn parse_coords2(value: &str) -> Result<(i32, i32), String> {
    let parts: Vec<_> = value
        .split(',')
        .map(|part| part.trim().parse::<i32>())
        .collect();
    match parts.as_slice() {
        [Ok(x), Ok(y)] => Ok((*x, *y)),
        _ => Err(format!("'{value}' is not two whole numbers 'x,y'")),
    }
}

pub fn parse_coords3(value: &str) -> Result<(f64, f64, f64), String> {
    let parts: Vec<_> = value
        .split(',')
        .map(|part| part.trim().parse::<f64>())
        .collect();
    match parts.as_slice() {
        [Ok(x), Ok(y), Ok(z)] if x.is_finite() && y.is_finite() && z.is_finite() => {
            Ok((*x, *y, *z))
        }
        _ => Err(format!("'{value}' is not three numbers 'x,y,z'")),
    }
}

/// Checks a value as written in a file against the kind of its setting.
/// `Ok(Some(..))` is a warning: the value is taken, but not as written.
pub fn check(kind: Kind, value: &str) -> Result<Option<String>, String> {
    let taken_as = |number: String| {
        Some(format!(
            "'{value}' is not a plain number; Naemon reads it as {number}, and checkd does the same"
        ))
    };
    match kind {
        Kind::Text | Kind::List => Ok(None),
        Kind::Flag => parse_flag(value).map(|_| None),
        Kind::Int => read_int(value).map(|(number, plain)| match plain {
            true => None,
            false => taken_as(number.to_string()),
        }),
        Kind::Float => read_float(value).map(|(number, plain)| match plain {
            true => None,
            false => taken_as(number.to_string()),
        }),
        Kind::Opts(allowed) => Opts::parse(value, allowed).map(|_| None),
        Kind::Coords2 => parse_coords2(value).map(|_| None),
        Kind::Coords3 => parse_coords3(value).map(|_| None),
    }
}

/// Splits a comma-separated list. Spaces around items and empty items are
/// dropped.
pub fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(String::from)
        .collect()
}
