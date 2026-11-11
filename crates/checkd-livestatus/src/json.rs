//! A small reader of JSON, enough for Livestatus answers.

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Num(f64),
    Str(String),
    List(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn as_str(&self) -> &str {
        match self {
            Json::Str(s) => s,
            _ => "",
        }
    }

    pub fn as_num(&self) -> f64 {
        match self {
            Json::Num(n) => *n,
            _ => 0.0,
        }
    }

    pub fn as_list(&self) -> &[Json] {
        match self {
            Json::List(items) => items,
            _ => &[],
        }
    }
}

pub fn parse(text: &str) -> Option<Json> {
    let chars: Vec<char> = text.chars().collect();
    let mut at = 0;
    read(&chars, &mut at)
}

fn skip(chars: &[char], at: &mut usize) {
    while *at < chars.len() && chars[*at].is_whitespace() {
        *at += 1;
    }
}

fn read(chars: &[char], at: &mut usize) -> Option<Json> {
    skip(chars, at);
    match chars.get(*at)? {
        '[' => {
            *at += 1;
            let mut items = Vec::new();
            loop {
                skip(chars, at);
                if chars.get(*at) == Some(&']') {
                    *at += 1;
                    return Some(Json::List(items));
                }
                items.push(read(chars, at)?);
                skip(chars, at);
                match chars.get(*at)? {
                    ',' => *at += 1,
                    ']' => {
                        *at += 1;
                        return Some(Json::List(items));
                    }
                    _ => return None,
                }
            }
        }
        '{' => {
            *at += 1;
            let mut items = Vec::new();
            loop {
                skip(chars, at);
                if chars.get(*at) == Some(&'}') {
                    *at += 1;
                    return Some(Json::Obj(items));
                }
                let Json::Str(key) = read(chars, at)? else { return None };
                skip(chars, at);
                if chars.get(*at) != Some(&':') {
                    return None;
                }
                *at += 1;
                items.push((key, read(chars, at)?));
                skip(chars, at);
                match chars.get(*at)? {
                    ',' => *at += 1,
                    '}' => {
                        *at += 1;
                        return Some(Json::Obj(items));
                    }
                    _ => return None,
                }
            }
        }
        '"' => {
            *at += 1;
            let mut text = String::new();
            loop {
                let c = *chars.get(*at)?;
                *at += 1;
                match c {
                    '"' => return Some(Json::Str(text)),
                    '\\' => {
                        let e = *chars.get(*at)?;
                        *at += 1;
                        match e {
                            'n' => text.push('\n'),
                            't' => text.push('\t'),
                            'r' => text.push('\r'),
                            'u' => {
                                let hex: String = chars.get(*at..*at + 4)?.iter().collect();
                                *at += 4;
                                text.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                            }
                            other => text.push(other),
                        }
                    }
                    c => text.push(c),
                }
            }
        }
        'n' => {
            *at += 4;
            Some(Json::Null)
        }
        _ => {
            let start = *at;
            while *at < chars.len() && !matches!(chars[*at], ',' | ']' | '}') && !chars[*at].is_whitespace() {
                *at += 1;
            }
            let text: String = chars[start..*at].iter().collect();
            text.parse().ok().map(Json::Num)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_json() {
        let value = parse("{\"data\":[[1,\"a\\\"b\",[2.5e+00]],\n[null]],\"total_count\":2}").unwrap();
        let Json::Obj(items) = value else { panic!() };
        assert_eq!(items[1], ("total_count".to_string(), Json::Num(2.0)));
    }
}
