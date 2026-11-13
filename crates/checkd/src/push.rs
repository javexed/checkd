//! Results pushed over HTTP, and the alarm when one does not come.
//!
//! A job that runs elsewhere (a backup, a cron job) calls
//! `/api/v1/push/<token>` when it is done; the token is a custom variable
//! of the service the result is for, `_PUSH_TOKEN`. With `_PUSH_EVERY
//! <seconds>` as well, the service goes CRITICAL when nothing was pushed
//! for that long. Both ways the service gets a passive result, by the
//! external command for one. See `docs/push.md`.

use std::collections::HashMap;

use checkd_core::engine::{Engine, Ms};

/// A token shorter than this is not taken: it stands in for a password.
const SHORTEST_TOKEN: usize = 16;

struct Target {
    host: String,
    service: String,
    /// Milliseconds within which a push is expected, if any.
    every: Option<Ms>,
    /// When the last push came, or the alarm for none was last raised.
    last: Ms,
}

#[derive(Default)]
pub struct Pushes {
    by_token: HashMap<String, Target>,
}

impl Pushes {
    /// The services of a configuration that take pushes. The second part
    /// is what to tell whoever starts checkd.
    pub fn new(engine: &Engine, now: Ms) -> (Pushes, Vec<String>) {
        let mut by_token = HashMap::new();
        let mut notes = Vec::new();
        for ((host, service), configured) in &engine.objects().services {
            let custom = |name: &str| configured.check.custom.iter().find(|(n, _)| n == name).map(|(_, v)| v.trim());
            let Some(token) = custom("PUSH_TOKEN") else { continue };
            let what = format!("{host};{service}");
            if token.len() < SHORTEST_TOKEN || !token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
                notes.push(format!(
                    "Warning: {what}: _PUSH_TOKEN is at least {SHORTEST_TOKEN} letters, digits, - and _; no push is taken for this service"
                ));
                continue;
            }
            let every = match custom("PUSH_EVERY") {
                None => None,
                Some(text) => match text.parse::<f64>() {
                    Ok(seconds) if seconds >= 1.0 => Some((seconds * 1000.0) as Ms),
                    _ => {
                        notes.push(format!("Warning: {what}: _PUSH_EVERY is a number of seconds, not '{text}'; no alarm is raised for this service"));
                        None
                    }
                },
            };
            let target = Target { host: host.clone(), service: service.clone(), every, last: now };
            if by_token.insert(token.to_string(), target).is_some() {
                notes.push(format!("Warning: {what}: another service has the same _PUSH_TOKEN; one of the two gets no pushes"));
            }
        }
        (Pushes { by_token }, notes)
    }

    pub fn any(&self) -> bool {
        !self.by_token.is_empty()
    }

    /// Whether any service is to raise an alarm when nothing is pushed.
    pub fn any_expected(&self) -> bool {
        self.by_token.values().any(|t| t.every.is_some())
    }

    /// A push: the external command it stands for, or nothing when the
    /// token is no service's.
    pub fn take(&mut self, token: &str, state: &str, output: &str, now: Ms) -> Result<Option<String>, String> {
        let Some(target) = self.by_token.get_mut(token) else { return Ok(None) };
        let code = match state.to_ascii_lowercase().as_str() {
            "" | "0" | "ok" => 0,
            "1" | "warning" => 1,
            "2" | "critical" => 2,
            "3" | "unknown" => 3,
            other => return Err(format!("state is ok, warning, critical or unknown (or 0 to 3), not '{other}'")),
        };
        target.last = now;
        Ok(Some(result(target, code, &one_line(output, "pushed"), now)))
    }

    /// The commands for the services whose push is overdue. Each is
    /// raised once for every time its span goes by with nothing pushed.
    pub fn overdue(&mut self, now: Ms) -> Vec<String> {
        let mut commands = Vec::new();
        for target in self.by_token.values_mut() {
            let Some(every) = target.every else { continue };
            if now - target.last > every {
                target.last = now;
                commands.push(result(target, 2, &format!("nothing was pushed for {} seconds", every / 1000), now));
            }
        }
        commands
    }
}

fn result(target: &Target, code: u8, output: &str, now: Ms) -> String {
    format!("[{}] PROCESS_SERVICE_CHECK_RESULT;{};{};{code};{output}", now.div_euclid(1000), target.host, target.service)
}

/// The first line of what was pushed, at a length a log line can take.
fn one_line(text: &str, or: &str) -> String {
    let line: String = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or(or).chars().take(1000).collect();
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pushes(every: Option<Ms>) -> Pushes {
        let target = Target { host: "nas".into(), service: "backup".into(), every, last: 0 };
        Pushes { by_token: HashMap::from([("abcdefghijklmnop".to_string(), target)]) }
    }

    #[test]
    fn a_push_is_a_passive_result() {
        let mut pushes = pushes(None);
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
    fn nothing_pushed_in_time_is_critical_once_a_span() {
        let mut pushes = pushes(Some(60_000));
        assert!(pushes.overdue(60_000).is_empty());
        assert_eq!(pushes.overdue(60_001), vec!["[60] PROCESS_SERVICE_CHECK_RESULT;nas;backup;2;nothing was pushed for 60 seconds"]);
        assert!(pushes.overdue(90_000).is_empty());
        assert_eq!(pushes.overdue(121_000).len(), 1);
        // A push starts the span again.
        pushes.take("abcdefghijklmnop", "ok", "", 130_000).unwrap();
        assert!(pushes.overdue(180_000).is_empty());
        assert_eq!(pushes.overdue(190_001).len(), 1);
    }
}
