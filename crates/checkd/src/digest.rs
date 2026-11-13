//! Several notifications to one contact, told as one.
//!
//! A contact with the custom variable `_DIGEST_COMMAND <command>` gets
//! its notifications a little late: the first is held for
//! `_DIGEST_WAIT` seconds (30 without it), and what else comes for the
//! contact in that time is held with it. One alone then goes out as it
//! is. Two or more go out as one run of the digest command, which is
//! told of them all in its environment. See `docs/digest.md`.

use std::collections::BTreeMap;

use checkd_core::engine::{Engine, Job, JobKind, Ms};

/// A contact's digest: the command line, and how long to wait.
struct Wanted {
    line: String,
    wait: Ms,
}

#[derive(Default)]
pub struct Digests {
    wanted: BTreeMap<String, Wanted>,
    /// By contact: when to send, and what is held.
    held: BTreeMap<String, (Ms, Vec<Job>)>,
    sent: u64,
}

/// What to do with what was held for a contact.
pub enum Release {
    /// One notification: run as it is.
    Single(Job),
    /// Several: these count as done, and this one command runs for them.
    Together { done: Vec<u64>, digest: Job, log: String },
}

impl Digests {
    /// The contacts of a configuration that want digests. The second part
    /// is what to tell whoever starts checkd.
    pub fn new(engine: &Engine) -> (Digests, Vec<String>) {
        let mut wanted = BTreeMap::new();
        let mut notes = Vec::new();
        let objects = engine.objects();
        for contact in objects.contacts.values() {
            let custom = |name: &str| contact.custom.iter().find(|(n, _)| n == name).map(|(_, v)| v.trim());
            let Some(command) = custom("DIGEST_COMMAND") else { continue };
            let Some(command) = objects.commands.get(command) else {
                notes.push(format!("Warning: contact {}: _DIGEST_COMMAND names no command '{command}'; its notifications go out one by one", contact.name));
                continue;
            };
            let wait = match custom("DIGEST_WAIT").map(str::parse::<f64>) {
                None => 30_000,
                Some(Ok(seconds)) if (1.0..=3600.0).contains(&seconds) => (seconds * 1000.0) as Ms,
                Some(_) => {
                    notes.push(format!("Warning: contact {}: _DIGEST_WAIT is 1 to 3600 seconds; 30 are taken", contact.name));
                    30_000
                }
            };
            // The macros a digest's command line may have: the contact's
            // and the resource file's. What the digest is about goes in
            // the environment, where no shell reads it as a command.
            let mut line = command.line.clone();
            for (macro_name, value) in [
                ("$CONTACTNAME$", contact.name.as_str()),
                ("$CONTACTALIAS$", contact.alias.as_str()),
                ("$CONTACTEMAIL$", contact.email.as_deref().unwrap_or("")),
                ("$CONTACTPAGER$", contact.pager.as_deref().unwrap_or("")),
            ] {
                line = line.replace(macro_name, value);
            }
            for n in 1..=256u32 {
                let name = format!("$USER{n}$");
                if line.contains(&name) {
                    line = line.replace(&name, engine.user_macro(n).unwrap_or(""));
                }
            }
            wanted.insert(contact.name.clone(), Wanted { line, wait });
        }
        (Digests { wanted, ..Digests::default() }, notes)
    }

    /// Takes a notification to hold, or gives it back to be run now.
    pub fn hold(&mut self, job: Job, now: Ms) -> Option<Job> {
        let contact = match (&job.kind, &job.about) {
            (JobKind::Notification, Some(about)) => about.contact.clone(),
            _ => return Some(job),
        };
        let Some(wanted) = self.wanted.get(&contact) else { return Some(job) };
        self.held.entry(contact).or_insert_with(|| (now + wanted.wait, Vec::new())).1.push(job);
        None
    }

    /// When the next held notifications are due.
    pub fn next(&self) -> Option<Ms> {
        self.held.values().map(|(at, _)| *at).min()
    }

    pub fn held(&self) -> usize {
        self.held.values().map(|(_, jobs)| jobs.len()).sum()
    }

    /// What is due at `now`.
    pub fn due(&mut self, now: Ms) -> Vec<Release> {
        let contacts: Vec<String> = self.held.iter().filter(|(_, (at, _))| *at <= now).map(|(c, _)| c.clone()).collect();
        let mut out = Vec::new();
        for contact in contacts {
            let Some((_, mut jobs)) = self.held.remove(&contact) else { continue };
            if jobs.len() == 1 {
                let mut job = jobs.remove(0);
                // Not held a second time.
                job.about = None;
                out.push(Release::Single(job));
                continue;
            }
            let wanted = &self.wanted[&contact];
            let abouts: Vec<_> = jobs.iter().filter_map(|job| job.about.as_deref()).collect();
            let mut kinds: Vec<(String, usize)> = Vec::new();
            for about in &abouts {
                match kinds.iter_mut().find(|(kind, _)| *kind == about.kind) {
                    Some((_, n)) => *n += 1,
                    None => kinds.push((about.kind.clone(), 1)),
                }
            }
            let summary = kinds.iter().map(|(kind, n)| format!("{n} {kind}")).collect::<Vec<_>>().join(", ");
            let lines = abouts
                .iter()
                .map(|a| format!("{}: {} is {}: {}", a.kind, a.what, a.state, a.output))
                .collect::<Vec<_>>()
                .join("\n");
            self.sent += 1;
            let digest = Job {
                // Not one of the engine's: its numbers count up from 1.
                id: u64::MAX - self.sent,
                kind: JobKind::Notification,
                obj: jobs[0].obj,
                line: wanted.line.clone(),
                timeout: jobs[0].timeout,
                about: None,
                env: vec![
                    ("CHECKD_DIGEST_CONTACT".to_string(), contact.clone()),
                    ("CHECKD_DIGEST_COUNT".to_string(), jobs.len().to_string()),
                    ("CHECKD_DIGEST_SUMMARY".to_string(), summary.clone()),
                    ("CHECKD_DIGEST_LINES".to_string(), lines),
                ],
            };
            let log = format!("NOTIFICATION DIGEST: {contact};{};{summary}", jobs.len());
            out.push(Release::Together { done: jobs.iter().map(|job| job.id).collect(), digest, log });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use checkd_core::engine::{About, Obj};

    fn notification(id: u64, contact: &str, kind: &str, what: &str) -> Job {
        Job {
            id,
            kind: JobKind::Notification,
            obj: Obj::Host(0),
            line: format!("notify {what}"),
            timeout: 30_000,
            about: Some(Box::new(About {
                contact: contact.into(),
                kind: kind.into(),
                what: what.into(),
                state: if kind == "RECOVERY" { "UP" } else { "DOWN" }.into(),
                output: "as said".into(),
            })),
            env: Vec::new(),
        }
    }

    fn digests() -> Digests {
        let wanted = BTreeMap::from([("ann".to_string(), Wanted { line: "tell ann".into(), wait: 30_000 })]);
        Digests { wanted, ..Digests::default() }
    }

    #[test]
    fn one_alone_goes_out_as_it_is_and_late() {
        let mut digests = digests();
        assert!(digests.hold(notification(1, "ann", "PROBLEM", "a"), 1000).is_none());
        assert_eq!(digests.next(), Some(31_000));
        assert!(digests.due(30_999).is_empty());
        let due = digests.due(31_000);
        assert!(matches!(&due[..], [Release::Single(job)] if job.id == 1 && job.line == "notify a" && job.about.is_none()));
        assert_eq!(digests.held(), 0);
    }

    #[test]
    fn several_go_out_as_one() {
        let mut digests = digests();
        for (id, kind, what) in [(1, "PROBLEM", "a"), (2, "PROBLEM", "a/disk"), (3, "RECOVERY", "b")] {
            assert!(digests.hold(notification(id, "ann", kind, what), 1000 * id as Ms).is_none());
        }
        // The wait counts from the first.
        let due = digests.due(31_000);
        let [Release::Together { done, digest, log }] = &due[..] else { panic!() };
        assert_eq!(done, &[1, 2, 3]);
        assert_eq!(digest.line, "tell ann");
        let env = |name: &str| digest.env.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str()).unwrap();
        assert_eq!(env("CHECKD_DIGEST_COUNT"), "3");
        assert_eq!(env("CHECKD_DIGEST_SUMMARY"), "2 PROBLEM, 1 RECOVERY");
        assert_eq!(env("CHECKD_DIGEST_LINES").lines().nth(1), Some("PROBLEM: a/disk is DOWN: as said"));
        assert_eq!(log, "NOTIFICATION DIGEST: ann;3;2 PROBLEM, 1 RECOVERY");
    }

    #[test]
    fn others_are_not_held() {
        let mut digests = digests();
        assert!(digests.hold(notification(1, "bob", "PROBLEM", "a"), 0).is_some());
        let mut check = notification(2, "ann", "PROBLEM", "a");
        check.kind = JobKind::ServiceCheck;
        assert!(digests.hold(check, 0).is_some());
    }
}
