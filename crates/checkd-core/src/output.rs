//! What a plugin printed, split into the short output, the long output and
//! the performance data.
//!
//! The rules are the plugin API's as the reference applies them:
//!
//! - empty lines before the first line are skipped;
//! - the first line is cut at its first `|`: the short output before it,
//!   performance data after it;
//! - the lines after it are long output, up to a line with a `|`. What
//!   comes after that `|`, and every line after it, is performance data;
//! - a `;` in the short output becomes a `:`, because the log and the
//!   status data use `;` between fields. The long output and the
//!   performance data keep theirs;
//! - lines of long output are joined by a backslash and an `n`, the parts
//!   of performance data by a space.

/// The three parts of a plugin's standard output.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Output {
    pub short: String,
    pub long: String,
    pub perfdata: String,
}

/// Splits what a plugin wrote to standard output.
pub fn parse(stdout: &str) -> Output {
    let mut lines = stdout.split('\n').skip_while(|line| line.trim().is_empty());
    let Some(first) = lines.next() else {
        return Output::default();
    };
    let mut perf: Vec<&str> = Vec::new();
    let short = match first.split_once('|') {
        Some((short, rest)) => {
            perf.push(rest);
            short
        }
        None => first,
    };
    let mut long: Vec<&str> = Vec::new();
    let mut in_perf = false;
    for line in lines {
        if in_perf {
            perf.push(line);
        } else if let Some((text, rest)) = line.split_once('|') {
            long.push(text);
            perf.push(rest);
            in_perf = true;
        } else {
            long.push(line);
        }
    }
    // The text ends with a newline as a rule, which leaves one empty piece.
    while long.last().is_some_and(|line| line.is_empty()) {
        long.pop();
    }
    let perf: Vec<&str> = perf
        .into_iter()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    Output {
        short: short.trim().replace(';', ":"),
        long: long.join("\\n"),
        perfdata: perf.join(" "),
    }
}

/// The short output for a check that printed nothing on standard output.
/// The reference words it one way for a service and another for a host.
pub fn no_output(stderr: &str, host: bool) -> String {
    let stderr = stderr.trim();
    if stderr.is_empty() {
        if host {
            "(No output returned from host check)".to_string()
        } else {
            "(No output returned from plugin)".to_string()
        }
    } else {
        let first = stderr.split('\n').next().unwrap_or("");
        format!("(No output on stdout) stderr: {first}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(short: &str, long: &str, perfdata: &str) -> Output {
        Output {
            short: short.to_string(),
            long: long.to_string(),
            perfdata: perfdata.to_string(),
        }
    }

    // Each case is a probe that was run through the reference, with what
    // it stored.
    #[test]
    fn one_line() {
        assert_eq!(parse("one\n"), out("one", "", ""));
        assert_eq!(parse("no newline at end"), out("no newline at end", "", ""));
        assert_eq!(
            parse("   leading and trailing   \n"),
            out("leading and trailing", "", "")
        );
        assert_eq!(
            parse("tab\there  and  double  spaces\n"),
            out("tab\there  and  double  spaces", "", "")
        );
    }

    #[test]
    fn leading_empty_lines() {
        assert_eq!(
            parse("\n\nafter two empty lines\n"),
            out("after two empty lines", "", "")
        );
    }

    #[test]
    fn semicolons() {
        assert_eq!(
            parse("has;semicolon and | pipe=1;2;3\n"),
            out("has:semicolon and", "", "pipe=1;2;3")
        );
        assert_eq!(
            parse("short;one | a=1;2\nlong;two with ; semi\nthird | b=1;2;3\nc=4;5\n"),
            out("short:one", "long;two with ; semi\\nthird ", "a=1;2 b=1;2;3 c=4;5")
        );
    }

    #[test]
    fn long_output_and_perfdata() {
        assert_eq!(
            parse("first line | p1=1\nlong line 2\nlong line 3 | p2=2\np3=3\n"),
            out("first line", "long line 2\\nlong line 3 ", "p1=1 p2=2 p3=3")
        );
        assert_eq!(parse("| only=1\n"), out("", "", "only=1"));
    }

    #[test]
    fn backslashes_stay() {
        assert_eq!(
            parse("back\\\\slash and \\n literal | perf\\n=1\nsecond\\tline\n"),
            out("back\\\\slash and \\n literal", "second\\tline", "perf\\n=1")
        );
    }

    #[test]
    fn nothing_on_stdout() {
        assert_eq!(parse(""), Output::default());
        assert_eq!(no_output("", false), "(No output returned from plugin)");
        assert_eq!(no_output(" \n", true), "(No output returned from host check)");
        assert_eq!(
            no_output("onlyerr\n", false),
            "(No output on stdout) stderr: onlyerr"
        );
    }
}
