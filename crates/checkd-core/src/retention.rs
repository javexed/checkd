//! Reads the state file of the reference version, `retention.dat`, into a
//! snapshot, so that a site that moves to checkd keeps its acknowledgements,
//! downtimes, comments and states.
//!
//! The file is blocks of `key=value` lines:
//!
//! ```text
//! service {
//! host_name=web
//! service_description=disk
//! current_state=2
//! ...
//! }
//! ```
//!
//! The block kinds and keys were read off files the reference wrote at the
//! end of runs on configs written for the purpose. Times in the file are
//! seconds since 1970. What checkd has no place for is skipped: the
//! `contact` blocks, and settings that are not the run-time switches
//! checkd knows.
//!
//! Comments that are not persistent are left out, as the reference does at
//! a restart, but for those of an acknowledgement or a downtime that is
//! still there.
//!
//! `write` goes the other way, for a site that goes back: the file the
//! reference reads at its start, from a snapshot.

use std::collections::BTreeMap;

use crate::snapshot::{AckState, CommentState, DowntimeState, ObjState, Snapshot};

/// One block: its kind and its values.
struct Block {
    kind: String,
    values: BTreeMap<String, String>,
}

fn blocks(text: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut current: Option<Block> = None;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(kind) = line.strip_suffix(" {") {
            current = Some(Block {
                kind: kind.trim().to_string(),
                values: BTreeMap::new(),
            });
        } else if line.trim() == "}" {
            out.extend(current.take());
        } else if let (Some(block), Some((key, value))) = (current.as_mut(), line.split_once('=')) {
            block.values.insert(key.to_string(), value.to_string());
        }
    }
    out
}

impl Block {
    fn text(&self, key: &str) -> String {
        self.values.get(key).cloned().unwrap_or_default()
    }
    fn int(&self, key: &str) -> i64 {
        let value = self.values.get(key).map(String::as_str).unwrap_or("0");
        value.trim().parse().unwrap_or_else(|_| value.trim().parse::<f64>().unwrap_or(0.0) as i64)
    }
    fn float(&self, key: &str) -> f64 {
        self.values.get(key).and_then(|v| v.trim().parse().ok()).unwrap_or(0.0)
    }
    fn flag(&self, key: &str) -> bool {
        self.int(key) != 0
    }
    fn ms(&self, key: &str) -> i64 {
        self.int(key) * 1000
    }
    /// A switch changed at run time, and the configuration's value then.
    /// Where the file has no `config:` line the two are the same.
    fn switch(&self, key: &str) -> Option<(bool, bool)> {
        let now = self.values.get(key)?.trim() != "0";
        let config = self
            .values
            .get(&format!("config:{key}"))
            .map_or(now, |value| value.trim() != "0");
        Some((now, config))
    }
}

/// Reads a retention file. The second part of the answer is what was
/// skipped, for the person who imports it.
pub fn read(text: &str) -> (Snapshot, Vec<String>) {
    let mut snapshot = Snapshot {
        notifications_enabled: true,
        ..Snapshot::default()
    };
    let mut notes = Vec::new();
    let mut acked: BTreeMap<(String, String), bool> = BTreeMap::new();
    let mut ack_comments: BTreeMap<(String, String), CommentState> = BTreeMap::new();

    let all = blocks(text);
    for block in &all {
        match block.kind.as_str() {
            "info" => {
                let version = block.text("version");
                if !version.is_empty() {
                    notes.push(format!("written by version {version}"));
                }
            }
            "program" => {
                if let Some((now, _)) = block.switch("enable_notifications") {
                    snapshot.notifications_enabled = now;
                }
                // The file has the next number to give out; checkd keeps
                // the last one given.
                snapshot.next_comment = block.int("next_comment_id").saturating_sub(1).max(0) as u64;
                snapshot.next_downtime = block.int("next_downtime_id").saturating_sub(1).max(0) as u64;
            }
            "host" | "service" => {
                let host = block.text("host_name");
                let service = (block.kind == "service").then(|| block.text("service_description"));
                let host_kind = service.is_none();
                let times: [i64; 4] = if host_kind {
                    [block.ms("last_time_up"), block.ms("last_time_down"), block.ms("last_time_unreachable"), 0]
                } else {
                    [
                        block.ms("last_time_ok"),
                        block.ms("last_time_warning"),
                        block.ms("last_time_critical"),
                        block.ms("last_time_unknown"),
                    ]
                };
                let history: Vec<u8> = block
                    .text("state_history")
                    .split(',')
                    .filter_map(|s| s.trim().parse().ok())
                    .collect();
                let (notifications, config_notifications) =
                    block.switch("notifications_enabled").unwrap_or((true, true));
                let (checks, config_checks) = block.switch("active_checks_enabled").unwrap_or((true, true));
                let number = block.int("current_notification_number").max(0) as u32;
                let state = block.int("current_state").clamp(0, if host_kind { 2 } else { 3 }) as u8;
                let key = (host.clone(), service.clone().unwrap_or_default());
                if block.flag("problem_has_been_acknowledged") {
                    acked.insert(key, block.int("acknowledgement_type") == 2);
                }
                snapshot.objects.push(ObjState {
                    host,
                    service,
                    state,
                    last_state: block.int("last_state") as u8,
                    last_hard_state: block.int("last_hard_state") as u8,
                    hard: block.int("state_type") == 1,
                    attempt: block.int("current_attempt").max(1) as u32,
                    checked: block.flag("has_been_checked"),
                    output: block.text("plugin_output"),
                    long_output: unescape(&block.text("long_plugin_output")),
                    perfdata: block.text("performance_data"),
                    last_check: block.ms("last_check"),
                    last_state_change: block.ms("last_state_change"),
                    last_hard_state_change: block.ms("last_hard_state_change"),
                    last_time: times,
                    flapping: block.flag("is_flapping"),
                    percent_change: block.float("percent_state_change"),
                    history,
                    event_id: block.int("current_event_id") as u64,
                    last_event_id: block.int("last_event_id") as u64,
                    problem_id: block.text("current_problem_id"),
                    last_problem_id: block.text("last_problem_id"),
                    notification_number: number,
                    last_notification: block.ms("last_notification"),
                    // The interval is the configuration's; the engine works
                    // the next time out from the last one.
                    next_notification: Some(0),
                    notified_state: if number > 0 { state } else { 0 },
                    notified: number > 0 && state != 0,
                    notification_id: block.text("current_notification_id"),
                    forced: false,
                    ack: None,
                    notifications_enabled: notifications,
                    active_checks_enabled: checks,
                    config_notifications_enabled: config_notifications,
                    config_active_checks_enabled: config_checks,
                    settings: Vec::new(),
                });
            }
            "hostcomment" | "servicecomment" => {
                let service = (block.kind == "servicecomment").then(|| block.text("service_description"));
                let expires = block.flag("expires").then(|| block.int("expire_time"));
                let comment = CommentState {
                    id: block.int("comment_id") as u64,
                    host: block.text("host_name"),
                    service,
                    entry_type: block.int("entry_type") as u8,
                    persistent: block.flag("persistent"),
                    entry: block.int("entry_time"),
                    author: block.text("author"),
                    text: block.text("comment_data"),
                    expires,
                };
                if comment.entry_type == 4 {
                    let key = (comment.host.clone(), comment.service.clone().unwrap_or_default());
                    // The newest acknowledgement comment speaks for it.
                    let newer = ack_comments.get(&key).is_none_or(|old| old.id < comment.id);
                    if newer {
                        ack_comments.insert(key, comment.clone());
                    }
                }
                snapshot.comments.push(comment);
            }
            "hostdowntime" | "servicedowntime" => {
                let service = (block.kind == "servicedowntime").then(|| block.text("service_description"));
                let fixed = block.flag("fixed");
                let active = block.flag("is_in_effect");
                let flex_start = block.int("flex_downtime_start");
                snapshot.downtimes.push(DowntimeState {
                    id: block.int("downtime_id") as u64,
                    host: block.text("host_name"),
                    service,
                    start: block.int("start_time"),
                    end: block.int("end_time"),
                    fixed,
                    trigger: block.int("triggered_by").max(0) as u64,
                    duration: block.int("duration"),
                    author: block.text("author"),
                    comment: block.text("comment"),
                    entry: block.int("entry_time"),
                    triggered: (!fixed && active).then_some(flex_start),
                    active,
                    comment_id: block.int("comment_id") as u64,
                });
            }
            "contact" => {}
            other => notes.push(format!("a block of kind '{other}' was skipped")),
        }
    }

    // Acknowledgements take their author and text from their comment.
    for object in &mut snapshot.objects {
        let key = (object.host.clone(), object.service.clone().unwrap_or_default());
        if let Some(&sticky) = acked.get(&key) {
            let comment = ack_comments.get(&key);
            object.ack = Some(AckState {
                author: comment.map(|c| c.author.clone()).unwrap_or_default(),
                comment: comment.map(|c| c.text.clone()).unwrap_or_default(),
                sticky,
                persistent: comment.is_some_and(|c| c.persistent),
                comment_id: comment.map_or(0, |c| c.id),
            });
        }
    }
    // Comments that a restart would drop.
    let kept_for: Vec<u64> = snapshot
        .objects
        .iter()
        .filter_map(|o| o.ack.as_ref().map(|a| a.comment_id))
        .chain(snapshot.downtimes.iter().map(|d| d.comment_id))
        .collect();
    let before = snapshot.comments.len();
    snapshot
        .comments
        .retain(|c| c.persistent || kept_for.contains(&c.id));
    let dropped = before - snapshot.comments.len();
    if dropped > 0 {
        notes.push(format!("{dropped} comments that do not persist were left out"));
    }
    (snapshot, notes)
}

/// A value the reference wrote with its newlines as `\n`.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (c, chars.clone().next()) {
            ('\\', Some('n')) => {
                out.push('\n');
                chars.next();
            }
            ('\\', Some('\\')) => {
                out.push('\\');
                chars.next();
            }
            _ => out.push(c),
        }
    }
    out
}

/// A `retention.dat` for the reference, from a snapshot: states, outputs,
/// acknowledgements, comments, downtimes, and the switches commands
/// changed. `now` is in seconds.
pub fn write(snapshot: &Snapshot, now: i64) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    // One line of a value: the reference keeps a newline as `\n`.
    let one = |text: &str| text.replace('\\', "\\\\").replace('\n', "\\n");
    let secs = |ms: i64| ms.div_euclid(1000);
    let _ = write!(out, "info {{\ncreated={now}\nversion=1.5.2\n}}\n");
    let program_changed = !snapshot.notifications_enabled;
    let _ = write!(
        out,
        "program {{\nmodified_host_attributes={m}\nmodified_service_attributes={m}\nenable_notifications={}\nnext_comment_id={}\nnext_downtime_id={}\n",
        snapshot.notifications_enabled as u8,
        snapshot.next_comment + 1,
        snapshot.next_downtime + 1,
        m = if program_changed { crate::engine::MOD_NOTIFICATIONS } else { 0 },
    );
    for setting in &snapshot.program {
        let _ = writeln!(out, "{}={}", setting.name, setting.value);
    }
    out.push_str("}\n");
    for o in &snapshot.objects {
        let host_kind = o.service.is_none();
        let mut modified: u32 = o.settings.iter().find(|s| s.name == "modified").and_then(|s| s.value.parse().ok()).unwrap_or(0);
        if o.notifications_enabled != o.config_notifications_enabled {
            modified |= crate::engine::MOD_NOTIFICATIONS;
        }
        if o.active_checks_enabled != o.config_active_checks_enabled {
            modified |= crate::engine::MOD_ACTIVE_CHECKS;
        }
        out.push_str(if host_kind { "host {\n" } else { "service {\n" });
        let _ = writeln!(out, "host_name={}", o.host);
        if let Some(service) = &o.service {
            let _ = writeln!(out, "service_description={service}");
        }
        let names: &[&str] = if host_kind { &["up", "down", "unreachable"] } else { &["ok", "warning", "critical", "unknown"] };
        let history = o.history.iter().map(u8::to_string).collect::<Vec<_>>().join(",");
        let _ = write!(
            out,
            "modified_attributes={modified}\nhas_been_checked={}\ncheck_type=0\ncurrent_state={}\nlast_state={}\nlast_hard_state={}\n\
             current_attempt={}\nstate_type={}\nplugin_output={}\nlong_plugin_output={}\nperformance_data={}\n\
             last_check={}\nlast_state_change={}\nlast_hard_state_change={}\nis_flapping={}\npercent_state_change={:.2}\nstate_history={history}\n\
             current_event_id={}\nlast_event_id={}\ncurrent_problem_id={}\nlast_problem_id={}\n\
             current_notification_number={}\ncurrent_notification_id={}\nlast_notification={}\n\
             notifications_enabled={}\nconfig:notifications_enabled={}\nactive_checks_enabled={}\nconfig:active_checks_enabled={}\n\
             problem_has_been_acknowledged={}\nacknowledgement_type={}\n",
            o.checked as u8,
            o.state,
            o.last_state,
            o.last_hard_state,
            o.attempt,
            o.hard as u8,
            one(&o.output),
            one(&o.long_output),
            one(&o.perfdata),
            secs(o.last_check),
            secs(o.last_state_change),
            secs(o.last_hard_state_change),
            o.flapping as u8,
            o.percent_change,
            o.event_id,
            o.last_event_id,
            o.problem_id,
            o.last_problem_id,
            o.notification_number,
            o.notification_id,
            secs(o.last_notification),
            o.notifications_enabled as u8,
            o.config_notifications_enabled as u8,
            o.active_checks_enabled as u8,
            o.config_active_checks_enabled as u8,
            o.ack.is_some() as u8,
            o.ack.as_ref().map_or(0, |a| if a.sticky { 2 } else { 1 }),
        );
        for (name, at) in names.iter().zip(o.last_time) {
            let _ = writeln!(out, "last_time_{name}={}", secs(at));
        }
        for setting in o.settings.iter().filter(|s| s.name != "modified") {
            let _ = writeln!(out, "{}={}", setting.name, one(&setting.value));
        }
        out.push_str("}\n");
    }
    for c in &snapshot.comments {
        let _ = writeln!(out, "{} {{", if c.service.is_some() { "servicecomment" } else { "hostcomment" });
        let _ = writeln!(out, "host_name={}", c.host);
        if let Some(service) = &c.service {
            let _ = writeln!(out, "service_description={service}");
        }
        let _ = write!(
            out,
            "entry_type={}\ncomment_id={}\nsource=0\npersistent={}\nentry_time={}\nexpires={}\nexpire_time={}\nauthor={}\ncomment_data={}\n}}\n",
            c.entry_type,
            c.id,
            c.persistent as u8,
            c.entry,
            c.expires.is_some() as u8,
            c.expires.unwrap_or(0),
            one(&c.author),
            one(&c.text),
        );
    }
    for d in &snapshot.downtimes {
        let _ = writeln!(out, "{} {{", if d.service.is_some() { "servicedowntime" } else { "hostdowntime" });
        let _ = writeln!(out, "host_name={}", d.host);
        if let Some(service) = &d.service {
            let _ = writeln!(out, "service_description={service}");
        }
        let _ = write!(
            out,
            "comment_id={}\ndowntime_id={}\nentry_time={}\nstart_time={}\nflex_downtime_start={}\nend_time={}\ntriggered_by={}\nfixed={}\nduration={}\nis_in_effect={}\nstart_notification_sent=0\nauthor={}\ncomment={}\n}}\n",
            d.comment_id,
            d.id,
            d.entry,
            d.start,
            d.triggered.unwrap_or(0),
            d.end,
            d.trigger,
            d.fixed as u8,
            d.duration,
            d.active as u8,
            one(&d.author),
            one(&d.comment),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "\
info {
created=1800000100
version=1.5.2
}
program {
enable_notifications=0
next_comment_id=5
next_downtime_id=3
}
host {
host_name=web
has_been_checked=1
current_state=1
last_state=1
last_hard_state=1
current_attempt=1
state_type=1
plugin_output=down; really
last_check=1800000090
last_state_change=1800000000
last_hard_state_change=1800000010
last_time_up=1799999990
last_time_down=1800000090
current_notification_number=2
last_notification=1800000080
problem_has_been_acknowledged=1
acknowledgement_type=2
is_flapping=0
percent_state_change=6.25
state_history=0,1,1
notifications_enabled=0
config:notifications_enabled=1
}
service {
host_name=web
service_description=disk
has_been_checked=1
current_state=0
current_attempt=1
state_type=1
plugin_output=fine
}
hostcomment {
host_name=web
entry_type=4
comment_id=2
persistent=1
entry_time=1800000050
expires=0
author=ann
comment_data=on it
}
servicecomment {
host_name=web
service_description=disk
entry_type=1
comment_id=3
persistent=0
entry_time=1800000060
author=bob
comment_data=gone at a restart
}
servicecomment {
host_name=web
service_description=disk
entry_type=1
comment_id=4
persistent=1
entry_time=1800000061
author=bob
comment_data=kept;with a semicolon
}
hostdowntime {
host_name=web
comment_id=1
downtime_id=2
entry_time=1800000001
start_time=1800000002
flex_downtime_start=0
end_time=1800003600
triggered_by=0
fixed=1
duration=3598
is_in_effect=1
author=ann
comment=work
}
contact {
contact_name=ann
}
";

    #[test]
    fn what_is_written_reads_back() {
        let (snapshot, _) = read(FILE);
        let (again, _) = read(&write(&snapshot, 1_800_000_200));
        assert_eq!(again.objects, snapshot.objects);
        assert_eq!(again.comments, snapshot.comments);
        assert_eq!(again.downtimes, snapshot.downtimes);
        assert_eq!((again.next_comment, again.next_downtime, again.notifications_enabled), (4, 2, false));
        // A long output keeps its lines.
        let mut long = snapshot.clone();
        long.objects[0].long_output = "one\ntwo".into();
        let text = write(&long, 0);
        assert!(text.contains("long_plugin_output=one\\ntwo\n"), "{text}");
        assert_eq!(read(&text).0.objects[0].long_output, "one\ntwo");
    }

    #[test]
    fn reads_a_file() {
        let (snapshot, notes) = read(FILE);
        assert!(!snapshot.notifications_enabled);
        assert_eq!((snapshot.next_comment, snapshot.next_downtime), (4, 2));
        let web = &snapshot.objects[0];
        assert_eq!((web.host.as_str(), web.service.as_deref()), ("web", None));
        assert_eq!((web.state, web.hard, web.attempt), (1, true, 1));
        assert_eq!(web.output, "down; really");
        assert_eq!(web.last_check, 1_800_000_090_000);
        assert_eq!(web.last_time[1], 1_800_000_090_000);
        assert_eq!(web.notification_number, 2);
        assert!(web.notified);
        assert!(!web.notifications_enabled);
        assert!(web.config_notifications_enabled);
        let ack = web.ack.as_ref().unwrap();
        assert_eq!((ack.author.as_str(), ack.comment.as_str(), ack.sticky), ("ann", "on it", true));
        let disk = &snapshot.objects[1];
        assert_eq!(disk.service.as_deref(), Some("disk"));
        assert!(disk.ack.is_none());
        // The comment that does not persist is dropped.
        let ids: Vec<u64> = snapshot.comments.iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![2, 4]);
        assert_eq!(snapshot.comments[1].text, "kept;with a semicolon");
        let downtime = &snapshot.downtimes[0];
        assert_eq!((downtime.id, downtime.active, downtime.fixed), (2, true, true));
        assert_eq!(downtime.end, 1_800_003_600);
        assert!(notes.iter().any(|n| n.contains("1.5.2")));
    }
}
