//! Reading a request: `GET <table>` and its header lines, or `COMMAND`.

/// A comparison of a column with a value written in the request.
#[derive(Clone, Debug, PartialEq)]
pub enum Filter {
    Cmp { column: String, op: Op, value: String },
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Not(Box<Filter>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    /// `~`: matches the regular expression.
    Match,
    NotMatch,
    /// `=~`: equal, case not counted.
    EqualIcase,
    NotEqualIcase,
    /// `~~`: matches, case not counted.
    MatchIcase,
    NotMatchIcase,
}

impl Op {
    fn parse(text: &str) -> Option<Op> {
        Some(match text {
            "=" => Op::Equal,
            "!=" => Op::NotEqual,
            "<" => Op::Less,
            "<=" => Op::LessOrEqual,
            ">" => Op::Greater,
            ">=" => Op::GreaterOrEqual,
            "~" => Op::Match,
            "!~" => Op::NotMatch,
            "=~" => Op::EqualIcase,
            "!=~" => Op::NotEqualIcase,
            "~~" => Op::MatchIcase,
            "!~~" => Op::NotMatchIcase,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aggregate {
    Sum,
    Min,
    Max,
    Avg,
    Std,
    SumInv,
    AvgInv,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stat {
    /// The number of rows that pass.
    Count(Filter),
    Aggregate(Aggregate, String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    WrappedJson,
    Csv,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    pub table: String,
    pub columns: Vec<String>,
    pub filter: Option<Filter>,
    pub stats: Vec<Stat>,
    pub sort: Vec<(String, bool)>,
    pub limit: Option<usize>,
    pub offset: usize,
    pub format: Format,
    pub fixed16: bool,
    pub keep_alive: bool,
    pub column_headers: Option<bool>,
    pub auth_user: Option<String>,
    /// CSV: between rows, between columns, between list items, between
    /// the parts of a list item.
    pub separators: [char; 4],
    /// Answered only once something happens, or the time is up.
    pub wait: Option<Wait>,
}

/// What a request waits for before it is answered: `WaitTrigger` with a
/// `WaitTimeout`. A request with a `WaitCondition` does not wait: the
/// conditions are not worked out here, and it is answered at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wait {
    pub trigger: Trigger,
    pub timeout_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    All,
    Check,
    State,
    Log,
    Downtime,
    Comment,
    Command,
    Program,
}

impl Trigger {
    fn parse(text: &str) -> Option<Trigger> {
        Some(match text {
            "all" => Trigger::All,
            "check" => Trigger::Check,
            "state" => Trigger::State,
            "log" => Trigger::Log,
            "downtime" => Trigger::Downtime,
            "comment" => Trigger::Comment,
            "command" => Trigger::Command,
            "program" => Trigger::Program,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    Get(Query),
    /// The command lines after `COMMAND `, their times included: every
    /// `COMMAND` line of the request, as Naemon carries them out.
    Command(Vec<String>),
}

/// A request that cannot be answered: the status code and why.
#[derive(Clone, Debug, PartialEq)]
pub struct Refused {
    pub code: u16,
    pub message: String,
    /// What the request asked for, as far as it got: the answer is still
    /// written in its format, with its header.
    pub fixed16: bool,
    pub keep_alive: bool,
}

/// How often one request may combine or negate filters (`And`, `Or`,
/// `Negate` and their `Stats` forms).
pub const MOST_COMBINED: usize = 500;

fn bad(message: impl Into<String>) -> Refused {
    Refused {
        code: 400,
        message: message.into(),
        fixed16: false,
        keep_alive: false,
    }
}

pub fn parse(text: &str) -> Result<Request, Refused> {
    let mut lines = text.lines().map(|line| line.trim_end_matches('\r'));
    let first = lines.next().unwrap_or("").trim();
    if first.starts_with("COMMAND ") {
        let commands = text
            .lines()
            .filter_map(|line| line.trim().strip_prefix("COMMAND "))
            .map(|command| command.trim().to_string())
            .collect();
        return Ok(Request::Command(commands));
    }
    let Some(table) = first.strip_prefix("GET ") else {
        return Err(bad(format!("Invalid request method '{first}'")));
    };
    let mut query = Query {
        table: table.trim().to_string(),
        columns: Vec::new(),
        filter: None,
        stats: Vec::new(),
        sort: Vec::new(),
        limit: None,
        offset: 0,
        format: Format::Csv,
        fixed16: false,
        keep_alive: false,
        column_headers: None,
        auth_user: None,
        separators: ['\n', ';', ',', '|'],
        wait: None,
    };
    let mut trigger: Option<Trigger> = None;
    let mut timeout_ms = 0u64;
    let mut conditional = false;
    let mut filters: Vec<Filter> = Vec::new();
    let mut stats: Vec<Stat> = Vec::new();
    let mut problem: Option<String> = None;
    // The answer to a faulty request has the response header only when
    // it was asked for before the fault.
    let mut fixed16_then = false;

    // Each header that combines or negates filters nests them one deeper,
    // and what is nested is walked by calling itself: a request is held to
    // a depth no stack minds.
    let mut combined = 0usize;
    for line in lines {
        if line.trim().is_empty() {
            break;
        }
        if line.starts_with("Negate") || line.starts_with("And") || line.starts_with("Or") || line.starts_with("Stats") && !line.starts_with("Stats:") {
            combined += 1;
            if combined > MOST_COMBINED {
                if problem.is_none() {
                    fixed16_then = query.fixed16;
                }
                problem.get_or_insert(format!("A request combines or negates filters at most {MOST_COMBINED} times"));
                break;
            }
        }
        let Some((header, value)) = line.split_once(':') else {
            if problem.is_none() {
                fixed16_then = query.fixed16;
            }
            problem.get_or_insert(format!("Undefined request header '{line}'"));
            continue;
        };
        let value = value.strip_prefix(' ').unwrap_or(value);
        let result: Result<(), String> = (|| {
            match header {
                "Columns" => query.columns = value.split_whitespace().map(str::to_string).collect(),
                "Filter" => filters.push(comparison(value)?),
                "And" | "Or" => {
                    let filter = combine(&mut filters, header, value)?;
                    filters.push(filter);
                }
                "Negate" => {
                    let last = filters.pop().ok_or("Negate on an empty filter stack")?;
                    filters.push(Filter::Not(Box::new(last)));
                }
                "Stats" => {
                    let mut parts = value.split_whitespace();
                    let first = parts.next().unwrap_or("");
                    let aggregate = match first {
                        "sum" => Some(Aggregate::Sum),
                        "min" => Some(Aggregate::Min),
                        "max" => Some(Aggregate::Max),
                        "avg" => Some(Aggregate::Avg),
                        "std" => Some(Aggregate::Std),
                        "suminv" => Some(Aggregate::SumInv),
                        "avginv" => Some(Aggregate::AvgInv),
                        _ => None,
                    };
                    match (aggregate, parts.next()) {
                        (Some(aggregate), Some(column)) => {
                            stats.push(Stat::Aggregate(aggregate, column.to_string()))
                        }
                        _ => stats.push(Stat::Count(comparison(value)?)),
                    }
                }
                "StatsAnd" | "StatsOr" => {
                    let n: usize = value.trim().parse().map_err(|_| format!("Invalid value for {header}"))?;
                    if n > stats.len() {
                        return Err(format!("Cannot combine {n} stats: there are only {}", stats.len()));
                    }
                    let parts: Vec<Stat> = stats.split_off(stats.len() - n);
                    let mut inner = Vec::new();
                    for part in parts {
                        match part {
                            Stat::Count(filter) => inner.push(filter),
                            Stat::Aggregate(..) => return Err("Cannot combine an aggregate".into()),
                        }
                    }
                    stats.push(Stat::Count(if header == "StatsAnd" {
                        Filter::And(inner)
                    } else {
                        Filter::Or(inner)
                    }));
                }
                "StatsNegate" => match stats.pop() {
                    Some(Stat::Count(filter)) => stats.push(Stat::Count(Filter::Not(Box::new(filter)))),
                    _ => return Err("StatsNegate needs a stats filter".into()),
                },
                "Sort" => {
                    let mut parts = value.split_whitespace();
                    let column = parts.next().ok_or("Sort needs a column")?;
                    let descending = matches!(parts.next(), Some("desc"));
                    query.sort.push((column.to_string(), descending));
                }
                "Limit" => {
                    query.limit = Some(value.trim().parse().map_err(|_| "Invalid value for Limit")?)
                }
                "Offset" => query.offset = value.trim().parse().map_err(|_| "Invalid value for Offset")?,
                "OutputFormat" => {
                    query.format = match value.trim() {
                        "json" => Format::Json,
                        "wrapped_json" => Format::WrappedJson,
                        "csv" => Format::Csv,
                        other => return Err(format!("Invalid output format '{other}'")),
                    }
                }
                "ResponseHeader" => query.fixed16 = value.trim() == "fixed16",
                "KeepAlive" => query.keep_alive = value.trim() == "on",
                "ColumnHeaders" => query.column_headers = Some(value.trim() == "on"),
                "AuthUser" => query.auth_user = Some(value.trim().to_string()),
                "Separators" => {
                    let numbers: Vec<u32> = value.split_whitespace().filter_map(|n| n.parse().ok()).collect();
                    for (place, n) in numbers.iter().take(4).enumerate() {
                        query.separators[place] = char::from_u32(*n).unwrap_or(';');
                    }
                }
                "WaitTrigger" => trigger = Trigger::parse(value.trim()),
                "WaitTimeout" => timeout_ms = value.trim().parse().unwrap_or(0),
                "WaitCondition" | "WaitConditionAnd" | "WaitConditionOr" | "WaitConditionNegate" => {
                    conditional = true
                }
                // Accepted, and not acted on.
                "Localtime" | "WaitObject" => {}
                _ => return Err(format!("Undefined request header '{line}'")),
            }
            Ok(())
        })();
        if let Err(message) = result {
            if problem.is_none() {
                fixed16_then = query.fixed16;
            }
            problem.get_or_insert(message);
        }
    }
    query.filter = match filters.len() {
        0 => None,
        1 => filters.pop(),
        _ => Some(Filter::And(filters)),
    };
    query.stats = stats;
    if let (Some(trigger), false, true) = (trigger, conditional, timeout_ms > 0) {
        query.wait = Some(Wait { trigger, timeout_ms });
    }
    if let Some(message) = problem {
        return Err(Refused {
            code: 400,
            message,
            fixed16: fixed16_then,
            keep_alive: query.keep_alive,
        });
    }
    Ok(Request::Get(query))
}

fn comparison(value: &str) -> Result<Filter, String> {
    let mut parts = value.splitn(3, ' ');
    let column = parts.next().unwrap_or("").to_string();
    let op_text = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").to_string();
    if op_text.is_empty() {
        return Err(format!("invalid filter header: missing operator after table '{column}'"));
    }
    let op = Op::parse(op_text).ok_or_else(|| format!("invalid filter header: unknown operator '{op_text}'"))?;
    Ok(Filter::Cmp { column, op, value: rest })
}

fn combine(filters: &mut Vec<Filter>, header: &str, value: &str) -> Result<Filter, String> {
    let n: usize = value.trim().parse().map_err(|_| format!("Invalid value for {header}"))?;
    if n > filters.len() {
        return Err(format!("Cannot combine {n} filters: there are only {}", filters.len()));
    }
    let parts = filters.split_off(filters.len() - n);
    Ok(if header == "And" { Filter::And(parts) } else { Filter::Or(parts) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_query() {
        let text = "GET hosts\nColumns: name state\nFilter: state = 1\nFilter: state = 2\nOr: 2\n\
                    Stats: state = 0\nStats: has_been_checked = 1\nStatsAnd: 2\nStats: sum latency\n\
                    Sort: name desc\nLimit: 5\nOutputFormat: wrapped_json\nResponseHeader: fixed16\n\n";
        let Request::Get(query) = parse(text).unwrap() else { panic!() };
        assert_eq!(query.table, "hosts");
        assert_eq!(query.columns, vec!["name", "state"]);
        assert!(matches!(query.filter, Some(Filter::Or(ref parts)) if parts.len() == 2));
        assert_eq!(query.stats.len(), 2);
        assert!(matches!(query.stats[1], Stat::Aggregate(Aggregate::Sum, ref c) if c == "latency"));
        assert_eq!(query.sort, vec![("name".to_string(), true)]);
        assert_eq!((query.limit, query.format, query.fixed16), (Some(5), Format::WrappedJson, true));
    }

    #[test]
    fn a_filter_value_keeps_its_spaces() {
        let Request::Get(query) = parse("GET log\nFilter: message ~~ a b  c\n").unwrap() else { panic!() };
        assert_eq!(
            query.filter,
            Some(Filter::Cmp { column: "message".into(), op: Op::MatchIcase, value: "a b  c".into() })
        );
    }

    #[test]
    fn a_wait_needs_a_trigger_and_a_time_and_no_condition() {
        let wait = |text: &str| match parse(text).unwrap() {
            Request::Get(query) => query.wait,
            Request::Command(_) => None,
        };
        assert_eq!(
            wait("GET status\nWaitTrigger: check\nWaitTimeout: 2000\n"),
            Some(Wait { trigger: Trigger::Check, timeout_ms: 2000 })
        );
        assert_eq!(wait("GET status\nWaitTrigger: check\n"), None);
        assert_eq!(wait("GET status\nWaitTimeout: 2000\n"), None);
        assert_eq!(wait("GET status\nWaitTrigger: sometimes\nWaitTimeout: 2000\n"), None);
        assert_eq!(
            wait("GET hosts\nWaitObject: a\nWaitCondition: state = 0\nWaitTrigger: check\nWaitTimeout: 2000\n"),
            None
        );
    }

    #[test]
    fn filters_nest_only_so_deep() {
        let nested = |n: usize| format!("GET hosts\nFilter: state = 0\n{}", "Negate:\n".repeat(n));
        assert!(parse(&nested(MOST_COMBINED)).is_ok());
        let refused = parse(&nested(MOST_COMBINED + 1)).unwrap_err();
        assert_eq!(refused.code, 400);
        // A hundred thousand of them are refused as readily, and nothing is built.
        assert!(parse(&nested(100_000)).is_err());
    }

    #[test]
    fn a_command() {
        assert_eq!(
            parse("COMMAND [1] DISABLE_NOTIFICATIONS\n").unwrap(),
            Request::Command(vec!["[1] DISABLE_NOTIFICATIONS".into()])
        );
        assert_eq!(
            parse("COMMAND [1] DISABLE_NOTIFICATIONS\nCOMMAND [1] DISABLE_FLAP_DETECTION\n").unwrap(),
            Request::Command(vec!["[1] DISABLE_NOTIFICATIONS".into(), "[1] DISABLE_FLAP_DETECTION".into()])
        );
    }
}
