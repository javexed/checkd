//! A service whose state comes from other hosts and services:
//!
//!     checkd:aggregate [--min <n>|<n>%] <member>...
//!
//! A member is a host (`web1`), a service (`web1/http`), or either with
//! a pattern or a host group in the host's place (`web*/http`,
//! `@webs/http`, `@webs`). The engine works it out itself, from what it
//! knows at that moment: nothing is run. See `docs/aggregate.md`.

use checkd_core::engine::{Engine, JobResult, Obj, Outcome, HOST_STATES, SERVICE_STATES};

/// Whether a command line is an aggregate.
pub fn is_aggregate(line: &str) -> bool {
    line.trim_start().split_whitespace().next() == Some("checkd:aggregate")
}

/// The words of a line; quotes keep a word with spaces together.
pub(crate) fn words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut begun = false;
    for c in line.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                begun = true;
            }
            None if c.is_whitespace() => {
                if begun || !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                    begun = false;
                }
            }
            None => word.push(c),
        }
    }
    if begun || !word.is_empty() {
        words.push(word);
    }
    words
}

fn answer(code: i32, text: String, perfdata: &str) -> JobResult {
    let word = ["OK", "WARNING", "CRITICAL", "UNKNOWN"][code as usize];
    let bar = if perfdata.is_empty() { String::new() } else { format!("|{perfdata}") };
    JobResult { outcome: Outcome::Exited(code), stdout: format!("AGGREGATE {word} - {text}{bar}\n"), stderr: String::new() }
}

/// The state of an aggregate, as the result of a check.
pub fn evaluate(engine: &Engine, line: &str) -> JobResult {
    let words = words(line.trim_start());
    let mut least: Option<String> = None;
    let mut members: Vec<Obj> = Vec::new();
    let mut at = 1;
    while at < words.len() {
        let word = &words[at];
        if word == "--min" {
            let Some(value) = words.get(at + 1) else { return answer(3, "--min needs a number".into(), "") };
            least = Some(value.clone());
            at += 2;
            continue;
        }
        let (hosts, services) = word.split_once('/').unwrap_or((word, ""));
        let found = engine.matching(hosts, services);
        if found.is_empty() {
            return answer(3, format!("'{word}' is no host or service of this configuration"), "");
        }
        for obj in found {
            if !members.contains(&obj) {
                members.push(obj);
            }
        }
        at += 1;
    }
    if members.is_empty() {
        return answer(3, "an aggregate needs members: hosts, or host/service".into(), "");
    }
    let total = members.len();
    let least = match least.as_deref() {
        None => None,
        Some(text) => {
            let (number, percent) = match text.strip_suffix('%') {
                Some(number) => (number, true),
                None => (text, false),
            };
            match number.parse::<usize>() {
                Ok(n) if percent && n <= 100 => Some((total * n).div_ceil(100)),
                Ok(n) if !percent && n <= total => Some(n),
                _ => return answer(3, format!("--min is a number up to the {total} members, or a percentage, not '{text}'"), ""),
            }
        }
    };

    // Each member by its hard state: one attempt that fails says nothing
    // yet. One not checked yet counts as OK.
    // Ranked: OK, WARNING, UNKNOWN, CRITICAL.
    let mut worst = 0;
    let mut not_ok: Vec<String> = Vec::new();
    for &obj in &members {
        let item = engine.item(obj);
        let state = if item.hard { item.state } else { item.last_hard_state };
        if !item.checked || state == 0 {
            continue;
        }
        let (name, code) = match obj {
            // A host that is down or cannot be reached is CRITICAL.
            Obj::Host(_) => (HOST_STATES[state as usize], 3),
            Obj::Service(_) => (SERVICE_STATES[state as usize], [0, 1, 3, 2][state as usize]),
        };
        worst = worst.max(code);
        not_ok.push(format!("{} ({name})", engine.log_name(obj).replace(';', "/")));
    }
    let ok = total - not_ok.len();
    let code = match least {
        // All of them: as bad as the worst, the rank as a state again.
        None => [0, 1, 3, 2][worst as usize],
        Some(n) if ok < n => 2,
        Some(_) if ok < total => 1,
        Some(_) => 0,
    };
    let mut text = format!("{ok} of {total} are OK");
    if let Some(n) = least {
        text.push_str(&format!(", {n} needed"));
    }
    if !not_ok.is_empty() {
        let shown = not_ok.iter().take(8).cloned().collect::<Vec<_>>().join(", ");
        let more = if not_ok.len() > 8 { format!(" and {} more", not_ok.len() - 8) } else { String::new() };
        // No semicolon: one ends the text of a result in the log.
        text.push_str(&format!(". Not OK: {shown}{more}"));
    }
    let limit = least.map(|n| format!("{n}:")).unwrap_or_default();
    answer(code, text, &format!("ok={ok};;{limit};0;{total}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> Engine {
        let mut objects = String::from(
            "define command {\n command_name ok\n command_line /bin/true\n}\n\
             define timeperiod {\n timeperiod_name always\n alias always\n monday 00:00-24:00\n}\n\
             define hostgroup {\n hostgroup_name webs\n members web1,web2,web3\n}\n",
        );
        for host in ["web1", "web2", "web3", "db1"] {
            objects.push_str(&format!(
                "define host {{\n host_name {host}\n address 192.0.2.1\n check_command ok\n max_check_attempts 1\n check_period always\n}}\n\
                 define service {{\n host_name {host}\n service_description http\n check_command ok\n max_check_attempts 1\n check_period always\n}}\n"
            ));
        }
        let dir = std::env::temp_dir().join(format!("checkd-aggregate-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("objects.cfg"), objects).unwrap();
        std::fs::write(dir.join("naemon.cfg"), "cfg_file=objects.cfg\n").unwrap();
        let loaded = checkd_config::load(&dir.join("naemon.cfg"), &Default::default());
        let (Some(main), Some(objects)) = (loaded.main, loaded.objects) else { panic!("{:?}", loaded.diags.errors) };
        Engine::new(objects, &main, Box::new(checkd_core::timeperiod::Fixed(0)), 1_800_000_000_000).0
    }

    fn state(engine: &Engine, line: &str) -> (i32, String) {
        let result = evaluate(engine, line);
        match result.outcome {
            Outcome::Exited(code) => (code, result.stdout.trim().to_string()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_state_of_the_members_together() {
        let mut engine = engine();
        let mut effects = Vec::new();
        let mut give = |engine: &mut Engine, line: &str| engine.command(&format!("[1800000000] {line}"), 1_800_000_000_000, &mut effects);
        // Nothing checked yet: nothing is wrong.
        assert_eq!(state(&engine, "checkd:aggregate web*/http").0, 0);
        give(&mut engine, "PROCESS_SERVICE_CHECK_RESULT;web2;http;1;slow");
        assert_eq!(
            state(&engine, "checkd:aggregate web*/http"),
            (1, "AGGREGATE WARNING - 2 of 3 are OK. Not OK: web2/http (WARNING)|ok=2;;;0;3".to_string())
        );
        give(&mut engine, "PROCESS_SERVICE_CHECK_RESULT;web3;http;2;down");
        // As bad as the worst; a group in the host's place; one named twice counts once.
        assert_eq!(state(&engine, "checkd:aggregate @webs/http web1/http").0, 2);
        // Enough of them: WARNING while some are not OK, CRITICAL when too few are.
        assert_eq!(state(&engine, "checkd:aggregate --min 1 web*/http").0, 1);
        assert_eq!(state(&engine, "checkd:aggregate --min 50% */http").0, 1);
        assert_eq!(state(&engine, "checkd:aggregate --min 2 web*/http").0, 2);
        assert_eq!(state(&engine, "checkd:aggregate --min 100% web1/http db1/http").0, 0);
        // A host that is down is CRITICAL.
        give(&mut engine, "PROCESS_HOST_CHECK_RESULT;web1;1;gone");
        assert_eq!(state(&engine, "checkd:aggregate @webs").0, 2);
        assert_eq!(state(&engine, "checkd:aggregate --min 2 @webs").0, 1);
        // What cannot be worked out.
        for line in ["checkd:aggregate", "checkd:aggregate nosuch", "checkd:aggregate --min 5 @webs", "checkd:aggregate --min x @webs", "checkd:aggregate web1/nosuch"] {
            assert_eq!(state(&engine, line).0, 3, "{line}");
        }
    }

    #[test]
    fn the_words_of_a_line() {
        assert_eq!(words("checkd:aggregate --min 2 web1/http 'db1/disk space' \"@webs/a b\""), [
            "checkd:aggregate",
            "--min",
            "2",
            "web1/http",
            "db1/disk space",
            "@webs/a b"
        ]);
        assert!(is_aggregate(" checkd:aggregate a") && !is_aggregate("checkd:aggregated") && !is_aggregate("checkd:tcp -H a"));
    }
}
