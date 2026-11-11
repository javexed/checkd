//! Compares checkd's Livestatus answers with the reference's.
//!
//! `runs/real` runs checkd on `runs/scenarios/livestatus-thruk` and puts
//! the queries of its `queries/` directory to it at the seconds of its
//! `ask` file; the reference got the same queries at the same seconds of
//! its own run. This test compares the two sets of answers, value by
//! value. It does nothing without `CHECKD_REAL`, or when the scenario has
//! no queries (they are made from a recording of Thruk, which stays out of
//! git: see `livestatus/README.md`).
//!
//! Some columns are not compared. Each is listed below with the reason.

use std::path::{Path, PathBuf};

/// Columns whose values are measured, or move while checks are stopped,
/// or describe the program rather than what it monitors.
const NOT_COMPARED: &[(&str, &str)] = &[
    ("execution_time", "measured"),
    ("latency", "measured"),
    ("last_update", "moves at every passed-over check while checks are stopped"),
    ("next_check", "moves at every passed-over check while checks are stopped"),
    ("is_executing", "a check may be running at the moment of the query"),
    ("check_options", "the reference's own bits for a check"),
    ("nagios_pid", "the program's"),
    ("program_version", "the program's"),
    ("livestatus_version", "the program's"),
    ("requests", "the program's counters"),
    ("requests_rate", "the program's counters"),
    ("connections", "the program's counters"),
    ("connections_rate", "the program's counters"),
    ("host_checks", "the program's counters"),
    ("host_checks_rate", "the program's counters"),
    ("service_checks", "the program's counters"),
    ("service_checks_rate", "the program's counters"),
    ("log_messages", "the program's counters"),
    ("log_messages_rate", "the program's counters"),
    ("cached_log_messages", "the program's counters"),
    ("neb_callbacks", "the program's counters"),
    ("neb_callbacks_rate", "the program's counters"),
    ("forks", "the program's counters"),
    ("forks_rate", "the program's counters"),
];

/// Where checkd answers otherwise on purpose: `runs/README.md`.
const ON_PURPOSE: &[(&str, &str)] = &[
    ("hostgroups.num_services_hard_ok", "the reference gives 0 for every host group"),
    ("hostgroups.num_services_hard_warn", "the reference gives 0 for every host group"),
    ("hostgroups.num_services_hard_crit", "the reference gives 0 for every host group"),
    ("hostgroups.num_services_hard_unknown", "the reference gives 0 for every host group"),
    ("hostgroups.worst_service_hard_state", "the reference gives 0 for every host group"),
];

/// The kinds of log line both programs write. Lines of other kinds, such
/// as each program's messages at its start, are not compared.
const LOG_KINDS: &[&str] = &[
    "HOST ALERT",
    "SERVICE ALERT",
    "INITIAL HOST STATE",
    "INITIAL SERVICE STATE",
    "HOST NOTIFICATION",
    "SERVICE NOTIFICATION",
    "HOST NOTIFICATION SUPPRESSED",
    "SERVICE NOTIFICATION SUPPRESSED",
    "HOST CONTACT NOTIFICATION SUPPRESSED",
    "SERVICE CONTACT NOTIFICATION SUPPRESSED",
    "HOST DOWNTIME ALERT",
    "SERVICE DOWNTIME ALERT",
    "HOST FLAPPING ALERT",
    "SERVICE FLAPPING ALERT",
    "EXTERNAL COMMAND",
    "SERVICE INFO",
    "HOST INFO",
];

use checkd_livestatus::json::{Json, parse};

struct Answer {
    code: String,
    rows: Vec<Vec<Json>>,
    body: String,
}

fn read_answer(path: &Path) -> Option<Answer> {
    let text = String::from_utf8_lossy(&std::fs::read(path).ok()?).into_owned();
    let code = text.get(..3).unwrap_or("").to_string();
    let body = text.get(16..).unwrap_or("").to_string();
    let rows = match parse(&body) {
        Some(Json::List(rows)) => rows,
        Some(Json::Obj(items)) => match items.into_iter().find(|(k, _)| k == "data") {
            Some((_, Json::List(rows))) => rows,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    let rows = rows
        .into_iter()
        .map(|row| match row {
            Json::List(cells) => cells,
            other => vec![other],
        })
        .collect();
    Some(Answer { code, rows, body })
}

/// What a query's answer columns are: the columns, then one per stat.
fn answer_columns(query: &str) -> (String, Vec<String>) {
    let mut lines = query.lines();
    let table = lines.next().unwrap_or("").trim_start_matches("GET ").trim().to_string();
    let mut columns = Vec::new();
    let mut stats: Vec<String> = Vec::new();
    for line in lines {
        let (header, value) = line.split_once(": ").unwrap_or((line, ""));
        match header {
            "Columns" => columns = value.split_whitespace().map(str::to_string).collect(),
            "Stats" => {
                let parts: Vec<&str> = value.split_whitespace().collect();
                let column = match parts.as_slice() {
                    ["sum" | "min" | "max" | "avg" | "std" | "suminv" | "avginv", column] => column.to_string(),
                    [column, ..] => format!("count:{column}"),
                    _ => String::new(),
                };
                stats.push(column);
            }
            "StatsAnd" | "StatsOr" => {
                let n: usize = value.trim().parse().unwrap_or(1);
                let parts = stats.split_off(stats.len().saturating_sub(n));
                stats.push(format!("count:{}", parts.join("+")));
            }
            _ => {}
        }
    }
    columns.extend(stats);
    (table, columns)
}

fn skipped(table: &str, column: &str) -> bool {
    let base = column.strip_prefix("host_").unwrap_or(column);
    let plain = |c: &str| NOT_COMPARED.iter().any(|(n, _)| *n == c);
    plain(column)
        || plain(base)
        || ON_PURPOSE.iter().any(|(n, _)| *n == format!("{table}.{column}"))
        // An aggregate over a column that is not compared.
        || (!column.starts_with("count:") && column != base && plain(base))
}

fn log_row_kept(row: &[Json]) -> bool {
    row.iter().any(|cell| match cell {
        Json::Str(text) => {
            let text = match text.strip_prefix('[') {
                Some(rest) => rest.split_once("] ").map_or(text.as_str(), |(_, t)| t),
                None => text.as_str(),
            };
            LOG_KINDS.iter().any(|kind| text == *kind || text.starts_with(&format!("{kind}: ")))
        }
        _ => false,
    })
}

fn same(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Num(x), Json::Num(y)) => (x - y).abs() <= 1e-9 * x.abs().max(1.0),
        _ => a == b,
    }
}

/// The scenarios that ask Livestatus queries: those with an `ask` file.
fn scenarios() -> Vec<(String, PathBuf)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runs/scenarios");
    let mut list: Vec<(String, PathBuf)> = std::fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .filter(|(_, p)| p.join("ask").exists())
        .collect();
    list.sort();
    list
}

#[test]
fn answers() {
    let Ok(real) = std::env::var("CHECKD_REAL") else { return };
    let mut problems = Vec::new();
    let mut compared = 0;
    for (scenario, dir) in scenarios() {
        compare_scenario(&scenario, &dir, Path::new(&real), &mut problems, &mut compared);
    }
    for problem in &problems {
        println!("  {problem}");
    }
    println!("livestatus answers: {compared} compared, {} differ", problems.len());
    assert!(problems.is_empty());
}

fn compare_scenario(scenario: &str, dir: &Path, real: &Path, problems: &mut Vec<String>, compared: &mut usize) {
    let queries = dir.join("queries");
    let reference = dir.join("expected/answers");
    let ours = real.join(scenario).join("out/answers");
    if !queries.is_dir() || !reference.is_dir() {
        println!("{scenario}: no queries; see livestatus/README.md");
        return;
    }
    if !ours.is_dir() {
        return;
    }
    let mut seconds: Vec<_> = std::fs::read_dir(&reference).unwrap().flatten().map(|e| e.file_name()).collect();
    seconds.sort();
    for second in seconds {
        let mut names: Vec<_> = std::fs::read_dir(reference.join(&second)).unwrap().flatten().map(|e| e.file_name()).collect();
        names.sort();
        for name in names {
            let query = std::fs::read_to_string(queries.join(&name)).unwrap_or_default();
            let (table, columns) = answer_columns(&query);
            let label = format!("{scenario} {}/{}", second.to_string_lossy(), name.to_string_lossy());
            let Some(want) = read_answer(&reference.join(&second).join(&name)) else { continue };
            let Some(have) = read_answer(&ours.join(&second).join(&name)) else {
                problems.push(format!("{label}: checkd gave no answer"));
                continue;
            };
            *compared += 1;
            if want.code != have.code {
                problems.push(format!("{label}: status {} where the reference gave {}", have.code, want.code));
                continue;
            }
            if want.rows.is_empty() && have.rows.is_empty() && want.body.trim() != have.body.trim() && want.code != "200" {
                problems.push(format!("{label}: '{}' where the reference said '{}'", have.body.trim(), want.body.trim()));
                continue;
            }
            let keep = |rows: &[Vec<Json>]| -> Vec<Vec<Json>> {
                rows.iter().filter(|r| table != "log" || log_row_kept(r)).cloned().collect()
            };
            let (mut want_rows, mut have_rows) = (keep(&want.rows), keep(&have.rows));
            if table == "log" {
                // The order of lines written in one second, and the second
                // itself within the allowance of the log comparison, depend
                // on real time: the rows are compared as a set, without
                // their times.
                let time_at = columns.iter().position(|c| c == "time");
                let plain = |rows: &mut Vec<Vec<Json>>| {
                    for row in rows.iter_mut() {
                        if let Some(at) = time_at {
                            if at < row.len() {
                                row[at] = Json::Null;
                            }
                        }
                        for cell in row.iter_mut() {
                            if let Json::Str(text) = cell {
                                if text.starts_with('[') {
                                    if let Some((_, rest)) = text.split_once("] ") {
                                        *text = rest.to_string();
                                    }
                                }
                            }
                        }
                    }
                    rows.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
                };
                plain(&mut want_rows);
                plain(&mut have_rows);
            }
            if want_rows.len() != have_rows.len() {
                problems.push(format!("{label}: {} rows where the reference gave {}", have_rows.len(), want_rows.len()));
                continue;
            }
            'rows: for (n, (a, b)) in want_rows.iter().zip(&have_rows).enumerate() {
                for (k, (x, y)) in a.iter().zip(b).enumerate() {
                    let column = columns.get(k).map(String::as_str).unwrap_or("?");
                    if skipped(&table, column) || same(x, y) {
                        continue;
                    }
                    problems.push(format!("{label}: row {n}, {table}.{column}: {y:?} where the reference gave {x:?}"));
                    break 'rows;
                }
            }
        }
    }
}
