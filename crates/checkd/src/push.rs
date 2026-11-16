//! Results pushed over HTTP.
//!
//! A job that runs elsewhere (a backup, a cron job) calls
//! `/api/v1/push/<token>` when it is done; the token is a custom variable
//! of the service the result is for, `_CHECKD_PUSH_TOKEN`. The service
//! gets a passive result, by the external command for one. That nothing
//! came in time is Naemon's freshness checking (`check_freshness`,
//! `freshness_threshold`), which the engine does. See `docs/push.md`.

use std::collections::HashMap;

use checkd_core::engine::{Engine, Ms};

/// The custom variable that holds a service's token, without its `_`.
pub const TOKEN_VARIABLE: &str = "CHECKD_PUSH_TOKEN";

/// A token shorter than this is not taken: it stands in for a password.
const SHORTEST_TOKEN: usize = 16;

struct Target {
    host: String,
    service: String,
}

#[derive(Default)]
pub struct Pushes {
    by_token: HashMap<String, Target>,
}

impl Pushes {
    /// The services of a configuration that take pushes. The second part
    /// is what to tell whoever starts checkd.
    pub fn new(engine: &Engine) -> (Pushes, Vec<String>) {
        let mut by_token = HashMap::new();
        let mut notes = Vec::new();
        for ((host, service), configured) in &engine.objects().services {
            let Some(token) = configured.check.custom.iter().find(|(n, _)| n == TOKEN_VARIABLE).map(|(_, v)| v.trim()) else {
                continue;
            };
            let what = format!("{host};{service}");
            if let Some(problem) = token_problem(token) {
                notes.push(format!("Warning: {what}: _{TOKEN_VARIABLE} {problem}; no push is taken for this service"));
                continue;
            }
            if by_token.insert(token.to_string(), Target { host: host.clone(), service: service.clone() }).is_some() {
                notes.push(format!("Warning: {what}: another service has the same _{TOKEN_VARIABLE}; one of the two gets no pushes"));
            }
            if !configured.check.passive_checks_enabled {
                notes.push(format!("Warning: {what}: has a _{TOKEN_VARIABLE} and takes no passive results (passive_checks_enabled 0): every push is refused"));
            }
        }
        (Pushes { by_token }, notes)
    }

    pub fn any(&self) -> bool {
        !self.by_token.is_empty()
    }

    /// A push: the external command it stands for, or nothing when the
    /// token is no service's.
    pub fn take(&mut self, token: &str, state: &str, output: &str, now: Ms) -> Result<Option<String>, String> {
        let Some(target) = self.by_token.get(token) else { return Ok(None) };
        let code = match state.to_ascii_lowercase().as_str() {
            "" | "0" | "ok" => 0,
            "1" | "warning" => 1,
            "2" | "critical" => 2,
            "3" | "unknown" => 3,
            other => return Err(format!("state is ok, warning, critical or unknown (or 0 to 3), not '{other}'")),
        };
        let output = one_line(output, "pushed");
        Ok(Some(format!("[{}] PROCESS_SERVICE_CHECK_RESULT;{};{};{code};{output}", now.div_euclid(1000), target.host, target.service)))
    }
}

/// Why a token is not taken, if it is not.
pub fn token_problem(token: &str) -> Option<String> {
    if token.len() < SHORTEST_TOKEN || !token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        Some(format!("is at least {SHORTEST_TOKEN} letters, digits, - and _"))
    } else {
        None
    }
}

/// The first line of what was pushed, at a length a log line can take.
fn one_line(text: &str, or: &str) -> String {
    text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or(or).chars().take(1000).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pushes() -> Pushes {
        let target = Target { host: "nas".into(), service: "backup".into() };
        Pushes { by_token: HashMap::from([("abcdefghijklmnop".to_string(), target)]) }
    }

    #[test]
    fn a_push_is_a_passive_result() {
        let mut pushes = pushes();
        assert_eq!(
            pushes.take("abcdefghijklmnop", "", "", 5000).unwrap().as_deref(),
            Some("[5] PROCESS_SERVICE_CHECK_RESULT;nas;backup;0;pushed")
        );
        assert_eq!(
            pushes.take("abcdefghijklmnop", "warning", "\n 3 files skipped \nmore", 5000).unwrap().as_deref(),
            Some("[5] PROCESS_SERVICE_CHECK_RESULT;nas;backup;1;3 files skipped")
        );
        assert_eq!(pushes.take("someone-elses-token", "", "", 5000).unwrap(), None);
        assert!(pushes.take("abcdefghijklmnop", "fine", "", 5000).is_err());
    }

    #[test]
    fn the_services_that_take_pushes() {
        use crate::testing::{NOW, commands, engine};
        let service = |name: &str, more: &str| {
            format!("define service {{\n host_name nas\n service_description {name}\n check_command ok\n max_check_attempts 1\n check_period always\n{more}}}\n")
        };
        let objects = format!(
            "define host {{\n host_name nas\n address 192.0.2.1\n check_command ok\n max_check_attempts 1\n check_period always\n}}\n{}{}{}{}{}",
            service("backup", " _CHECKD_PUSH_TOKEN 0123456789abcdef\n"),
            service("short", " _CHECKD_PUSH_TOKEN abc\n"),
            service("deaf", " _CHECKD_PUSH_TOKEN deaf-deaf-deaf-deaf\n passive_checks_enabled 0\n"),
            service("twin", " _CHECKD_PUSH_TOKEN 0123456789abcdef\n"),
            service("plain", ""),
        );
        let mut engine = engine(&objects, "");
        let (mut pushes, notes) = Pushes::new(&engine);
        let said = |part: &str| notes.iter().any(|note| note.contains(part));
        assert!(said("nas;short: _CHECKD_PUSH_TOKEN is at least 16"), "{notes:?}");
        assert!(said("nas;deaf: has a _CHECKD_PUSH_TOKEN and takes no passive results"), "{notes:?}");
        assert!(said("another service has the same _CHECKD_PUSH_TOKEN"), "{notes:?}");
        assert_eq!(notes.len(), 3, "{notes:?}");
        // The short one is not taken; the others are, and a push is a result.
        assert_eq!(pushes.take("abc", "", "", NOW).unwrap(), None);
        let line = pushes.take("deaf-deaf-deaf-deaf", "critical", "no", NOW).unwrap().unwrap();
        assert!(line.ends_with("PROCESS_SERVICE_CHECK_RESULT;nas;deaf;2;no"), "{line}");
        let line = pushes.take("0123456789abcdef", "warning", "slow", NOW).unwrap().unwrap();
        let target = if line.contains(";backup;") { "backup" } else { "twin" };
        commands(&mut engine, NOW, &[line]);
        let item = engine.item(engine.service("nas", target).unwrap());
        assert_eq!((item.state, item.output.short.as_str()), (1, "slow"));
        // Nothing to take without a token.
        let none = crate::testing::engine(&objects.replace("_CHECKD_PUSH_TOKEN", "_OTHER"), "");
        assert!(!Pushes::new(&none).0.any());
    }

    #[test]
    fn tokens_that_are_not_taken() {
        assert!(token_problem("short").is_some());
        assert!(token_problem("sixteen chars !!").is_some());
        assert!(token_problem("0123456789abcdef").is_none());
        assert!(token_problem("a-b_c-d_e-f_g-h_").is_none());
    }
}
