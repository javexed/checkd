//! Livestatus: the query language Thruk and other tools use to read the
//! state of a monitoring engine, answered from checkd's engine.
//!
//! A request is `GET <table>` with header lines, or `COMMAND <command>`.
//! [`handle`] answers a request against a [`Source`]; a command is handed
//! back for the caller to give the engine.
//!
//! The behaviour is the reference's, learned from its answers to queries,
//! and first of all to the queries Thruk was recorded sending for its
//! pages (`livestatus/` at the top of the repository).

mod auth;
mod columns;
pub mod json;
pub mod request;
pub mod statehist;
pub mod tables;
pub mod value;

use std::collections::BTreeMap;

use regex::RegexBuilder;

pub use request::{Filter, Format, Op, Query, Request};
pub use tables::{Program, Row, Source};
use value::Value;

/// What to send back, and what else to do.
#[derive(Debug, Default)]
pub struct Response {
    pub bytes: Vec<u8>,
    /// Whether the connection stays open for another request.
    pub keep_alive: bool,
    /// A command for the engine, from a `COMMAND` request.
    pub command: Option<String>,
}

/// What a request waits for before it is to be answered, if anything.
/// The one who holds the engine does the waiting, and then calls `handle`.
pub fn wait_of(text: &str) -> Option<request::Wait> {
    // Most requests wait for nothing, and are read once only.
    if !text.contains("WaitTrigger") {
        return None;
    }
    match request::parse(text) {
        Ok(Request::Get(query)) => query.wait,
        _ => None,
    }
}

/// Answers one request.
pub fn handle(source: &Source, text: &str) -> Response {
    match request::parse(text) {
        Ok(Request::Command(command)) => Response {
            bytes: Vec::new(),
            keep_alive: false,
            command: Some(command),
        },
        Ok(Request::Get(query)) => match answer(source, &query) {
            Ok(body) => respond(200, body, query.fixed16, query.keep_alive),
            Err((code, message)) => respond(code, format!("{message}\n"), query.fixed16, query.keep_alive),
        },
        Err(refused) => respond(refused.code, format!("{}\n", refused.message), refused.fixed16, refused.keep_alive),
    }
}

fn respond(code: u16, body: String, fixed16: bool, keep_alive: bool) -> Response {
    let mut bytes = Vec::new();
    if fixed16 {
        bytes.extend(format!("{code:03} {:>11}\n", body.len()).into_bytes());
    }
    bytes.extend(body.into_bytes());
    Response {
        bytes,
        keep_alive,
        command: None,
    }
}

type Failure = (u16, String);

fn answer(source: &Source, query: &Query) -> Result<String, Failure> {
    // The state history is worked out for the time the filters ask about.
    let windowed;
    let source = if query.table == "statehist" {
        windowed = Source {
            window: Some(window_of(query.filter.as_ref(), source.now.div_euclid(1000))),
            ..*source
        };
        &windowed
    } else {
        source
    };
    let rows = tables::rows(source, &query.table)
        .ok_or_else(|| (404, format!("Invalid GET request, no such table '{}'", query.table)))?;
    let columns: Vec<String> = if query.columns.is_empty() && query.stats.is_empty() {
        all_columns(source, &query.table, rows.first())
    } else {
        query.columns.clone()
    };
    // A column the table does not have is answered with an empty
    // string, as the reference does.
    let mut rows = rows;
    if query.table == "services" && pinned_host(query.filter.as_ref()) {
        // Asked for the services of one host, the reference goes through
        // the host's own list, which is in the reverse order of the names.
        rows.reverse();
    }
    let mut kept = Vec::new();
    for row in rows {
        if let Some(user) = &query.auth_user {
            if !auth::visible(source, &row, user) {
                continue;
            }
        }
        if let Some(filter) = &query.filter {
            if !passes(source, &row, filter)? {
                continue;
            }
        }
        kept.push(row);
    }

    let lines: Vec<Vec<Value>> = if query.stats.is_empty() {
        sort(source, &mut kept, &query.sort)?;
        kept.iter()
            .map(|row| columns.iter().map(|c| tables::get(source, row, c).unwrap_or(Value::str(""))).collect())
            .collect()
    } else {
        stats(source, &kept, &columns, query)?
    };
    let total = lines.len();
    let lines: Vec<Vec<Value>> = lines
        .into_iter()
        .skip(query.offset)
        .take(query.limit.unwrap_or(usize::MAX))
        .collect();
    let headers = query.column_headers.unwrap_or(false) && query.stats.is_empty();
    Ok(write(query, &columns, &lines, total, headers))
}

fn all_columns(source: &Source, table: &str, row: Option<&Row>) -> Vec<String> {
    const SOME: &[&str] = &["name", "alias", "state", "plugin_output", "description", "host_name"];
    let Some(row) = row else { return Vec::new() };
    SOME.iter()
        .filter(|c| tables::get(source, row, c).is_some())
        .map(|c| c.to_string())
        .chain(if table == "columns" { vec!["name".into(), "table".into(), "type".into()] } else { vec![] })
        .collect()
}

/// Whether a table has a column. A table without rows is asked through a
/// row it could have.
/// The time a query's `time` filters keep: from the lower bound to the
/// upper, less a second for `<`.
fn window_of(filter: Option<&Filter>, now: i64) -> statehist::Window {
    let mut window = statehist::Window { from: 0, until: now };
    let walk = |f: &Filter, w: &mut statehist::Window| {
        if let Filter::Cmp { column, op, value } = f {
            if column == "time" {
                let n: i64 = value.trim().parse().unwrap_or(0);
                match op {
                    Op::GreaterOrEqual => w.from = w.from.max(n),
                    Op::Greater => w.from = w.from.max(n + 1),
                    Op::Less => w.until = w.until.min(n - 1),
                    Op::LessOrEqual => w.until = w.until.min(n),
                    _ => {}
                }
            }
        }
    };
    match filter {
        Some(Filter::And(parts)) => parts.iter().for_each(|p| walk(p, &mut window)),
        Some(other) => walk(other, &mut window),
        None => {}
    }
    window
}

/// Whether a filter keeps the services of one host only, by its name.
fn pinned_host(filter: Option<&Filter>) -> bool {
    match filter {
        Some(Filter::Cmp { column, op: Op::Equal, .. }) => column == "host_name",
        Some(Filter::And(parts)) => parts.iter().any(|p| pinned_host(Some(p))),
        _ => false,
    }
}

fn passes(source: &Source, row: &Row, filter: &Filter) -> Result<bool, Failure> {
    Ok(match filter {
        Filter::And(parts) => {
            for part in parts {
                if !passes(source, row, part)? {
                    return Ok(false);
                }
            }
            true
        }
        Filter::Or(parts) => {
            for part in parts {
                if passes(source, row, part)? {
                    return Ok(true);
                }
            }
            false
        }
        Filter::Not(inner) => !passes(source, row, inner)?,
        Filter::Cmp { column, op, value } => {
            let have = tables::get(source, row, column).unwrap_or(Value::str(""));
            compare(&have, *op, value)
        }
    })
}

fn regex(pattern: &str, icase: bool) -> Option<regex::Regex> {
    RegexBuilder::new(pattern).case_insensitive(icase).build().ok()
}

/// A comparison as Livestatus makes it, by the type of the column.
pub fn compare(have: &Value, op: Op, want: &str) -> bool {
    match have {
        Value::List(items) => {
            let texts: Vec<String> = items.iter().map(Value::text).collect();
            let contains = |icase: bool| {
                texts.iter().any(|t| if icase { t.eq_ignore_ascii_case(want) } else { t == want })
            };
            match op {
                Op::Equal => want.is_empty() && texts.is_empty(),
                Op::NotEqual => !(want.is_empty() && texts.is_empty()),
                Op::GreaterOrEqual => contains(false),
                Op::Less => !contains(false),
                Op::LessOrEqual => contains(true),
                Op::Greater => !contains(true),
                Op::Match | Op::MatchIcase => match regex(want, op == Op::MatchIcase) {
                    Some(re) => texts.iter().any(|t| re.is_match(t)),
                    None => false,
                },
                Op::NotMatch | Op::NotMatchIcase => match regex(want, op == Op::NotMatchIcase) {
                    Some(re) => !texts.iter().any(|t| re.is_match(t)),
                    None => false,
                },
                Op::EqualIcase => contains(true),
                Op::NotEqualIcase => !contains(true),
            }
        }
        Value::Int(_) | Value::Float(_) | Value::Time(_) => {
            let a = have.number();
            let b: f64 = want.trim().parse().unwrap_or(0.0);
            match op {
                Op::Equal | Op::EqualIcase => a == b,
                Op::NotEqual | Op::NotEqualIcase => a != b,
                Op::Less => a < b,
                Op::LessOrEqual => a <= b,
                Op::Greater => a > b,
                Op::GreaterOrEqual => a >= b,
                Op::Match | Op::MatchIcase | Op::NotMatch | Op::NotMatchIcase => {
                    string_compare(&have.text(), op, want)
                }
            }
        }
        Value::Str(text) => string_compare(text, op, want),
        Value::Dict(items) => {
            // `custom_variables = NAME value`
            let (name, value) = want.split_once(' ').unwrap_or((want, ""));
            match items.iter().find(|(k, _)| k == name) {
                Some((_, v)) => string_compare(v, op, value),
                None => matches!(op, Op::NotEqual | Op::NotMatch | Op::NotMatchIcase | Op::NotEqualIcase),
            }
        }
    }
}

fn string_compare(have: &str, op: Op, want: &str) -> bool {
    match op {
        Op::Equal => have == want,
        Op::NotEqual => have != want,
        Op::Less => have < want,
        Op::LessOrEqual => have <= want,
        Op::Greater => have > want,
        Op::GreaterOrEqual => have >= want,
        Op::EqualIcase => have.eq_ignore_ascii_case(want),
        Op::NotEqualIcase => !have.eq_ignore_ascii_case(want),
        Op::Match => regex(want, false).is_some_and(|re| re.is_match(have)),
        Op::NotMatch => !regex(want, false).is_some_and(|re| re.is_match(have)),
        Op::MatchIcase => regex(want, true).is_some_and(|re| re.is_match(have)),
        Op::NotMatchIcase => !regex(want, true).is_some_and(|re| re.is_match(have)),
    }
}

fn sort(source: &Source, rows: &mut [Row], keys: &[(String, bool)]) -> Result<(), Failure> {
    if keys.is_empty() {
        return Ok(());
    }
    let mut keyed: Vec<(Vec<Value>, Row)> = Vec::new();
    for row in rows.iter() {
        let mut values = Vec::new();
        for (column, _) in keys {
            values.push(tables::get(source, row, column).unwrap_or(Value::str("")));
        }
        keyed.push((values, row.clone()));
    }
    keyed.sort_by(|a, b| {
        for (n, (_, descending)) in keys.iter().enumerate() {
            let order = order_of(&a.0[n], &b.0[n]);
            let order = if *descending { order.reverse() } else { order };
            if order != std::cmp::Ordering::Equal {
                return order;
            }
        }
        std::cmp::Ordering::Equal
    });
    for (place, (_, row)) in keyed.into_iter().enumerate() {
        rows[place] = row;
    }
    Ok(())
}

fn order_of(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a, b) {
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        (Value::List(_), _) | (_, Value::List(_)) => a.text().cmp(&b.text()),
        _ => a.number().partial_cmp(&b.number()).unwrap_or(std::cmp::Ordering::Equal),
    }
}

fn stats(source: &Source, rows: &[Row], columns: &[String], query: &Query) -> Result<Vec<Vec<Value>>, Failure> {
    // Rows by the values of the columns they are grouped by.
    let mut groups: BTreeMap<Vec<String>, (Vec<Value>, Vec<&Row>)> = BTreeMap::new();
    for row in rows {
        let values: Vec<Value> = columns
            .iter()
            .map(|c| tables::get(source, row, c).unwrap_or(Value::str("")))
            .collect();
        let key: Vec<String> = values.iter().map(Value::text).collect();
        groups.entry(key).or_insert_with(|| (values, Vec::new())).1.push(row);
    }
    if columns.is_empty() && groups.is_empty() {
        groups.insert(Vec::new(), (Vec::new(), Vec::new()));
    }
    let mut out = Vec::new();
    for (_, (values, members)) in groups {
        let mut line = values;
        for stat in &query.stats {
            line.push(match stat {
                request::Stat::Count(filter) => {
                    let mut n = 0;
                    for row in &members {
                        if passes(source, row, filter)? {
                            n += 1;
                        }
                    }
                    Value::Int(n)
                }
                request::Stat::Aggregate(kind, column) => {
                    let mut numbers = Vec::new();
                    for row in &members {
                        let value = tables::get(source, row, column).unwrap_or(Value::Int(0));
                        numbers.push(value.number());
                    }
                    aggregate(*kind, &numbers)
                }
            });
        }
        out.push(line);
    }
    Ok(out)
}

fn aggregate(kind: request::Aggregate, numbers: &[f64]) -> Value {
    use request::Aggregate::*;
    let n = numbers.len() as f64;
    let sum: f64 = numbers.iter().sum();
    Value::Float(match kind {
        Sum => sum,
        Min => numbers.iter().copied().fold(f64::INFINITY, f64::min).min(if numbers.is_empty() { 0.0 } else { f64::INFINITY }),
        Max => numbers.iter().copied().fold(f64::NEG_INFINITY, f64::max).max(if numbers.is_empty() { 0.0 } else { f64::NEG_INFINITY }),
        Avg => if numbers.is_empty() { 0.0 } else { sum / n },
        Std => {
            if numbers.is_empty() {
                0.0
            } else {
                let mean = sum / n;
                (numbers.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n).sqrt()
            }
        }
        SumInv => numbers.iter().map(|x| if *x == 0.0 { 0.0 } else { 1.0 / x }).sum(),
        AvgInv => {
            if numbers.is_empty() {
                0.0
            } else {
                numbers.iter().map(|x| if *x == 0.0 { 0.0 } else { 1.0 / x }).sum::<f64>() / n
            }
        }
    })
}

fn write(query: &Query, columns: &[String], lines: &[Vec<Value>], total: usize, headers: bool) -> String {
    let mut out = String::new();
    match query.format {
        Format::Json | Format::WrappedJson => {
            let wrapped = query.format == Format::WrappedJson;
            if wrapped {
                out.push_str("{\"data\":");
            }
            out.push('[');
            let mut first = true;
            if headers {
                Value::strs(columns.iter().cloned()).write_json(&mut out);
                first = false;
            }
            for line in lines {
                if !first {
                    out.push_str(",\n");
                }
                first = false;
                Value::List(line.clone()).write_json(&mut out);
            }
            out.push(']');
            if wrapped {
                out.push_str(&format!(",\"total_count\":{total}}}"));
            }
            out.push('\n');
        }
        Format::Csv => {
            let [row_sep, col_sep, list_sep, part_sep] = query.separators;
            let cell = |value: &Value| -> String {
                match value {
                    Value::List(items) => items
                        .iter()
                        .map(|item| match item {
                            Value::List(parts) => parts.iter().map(Value::text).collect::<Vec<_>>().join(&part_sep.to_string()),
                            other => other.text(),
                        })
                        .collect::<Vec<_>>()
                        .join(&list_sep.to_string()),
                    other => other.text(),
                }
            };
            if headers {
                out.push_str(&columns.join(&col_sep.to_string()));
                out.push(row_sep);
            }
            for line in lines {
                out.push_str(&line.iter().map(cell).collect::<Vec<_>>().join(&col_sep.to_string()));
                out.push(row_sep);
            }
        }
    }
    out
}
