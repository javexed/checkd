//! The engine's state as plain values, to keep it over a restart.
//!
//! [`Engine::snapshot`] takes it out, [`Engine::restore`] puts it back in
//! an engine made from the same or a changed configuration. Objects are
//! matched by name; what has no object any more is dropped. A store writes
//! a snapshot's parts where it likes.
//!
//! Restore before [`Engine::start`]: the start plans the checks, and
//! knows then which objects were checked before.

use checkd_config::model::Checkable;

use crate::engine::{Engine, Ms, Obj};
use crate::settings::Settings;
use crate::flap::History;
use crate::notify::{Ack, Comment, Downtime, Timer};
use crate::output::Output;

/// The state of one host or service.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ObjState {
    pub host: String,
    /// `None` for a host.
    pub service: Option<String>,
    pub state: u8,
    pub last_state: u8,
    pub last_hard_state: u8,
    pub hard: bool,
    pub attempt: u32,
    pub checked: bool,
    pub output: String,
    pub long_output: String,
    pub perfdata: String,
    /// Milliseconds since the epoch, 0 for never.
    pub last_check: Ms,
    pub last_state_change: Ms,
    pub last_hard_state_change: Ms,
    pub last_time: [Ms; 4],
    pub flapping: bool,
    pub percent_change: f64,
    /// The flap history, oldest first.
    pub history: Vec<u8>,
    pub event_id: u64,
    pub last_event_id: u64,
    pub problem_id: String,
    pub last_problem_id: String,
    pub notification_number: u32,
    pub last_notification: Ms,
    /// When the next notification may go out. `None` for never again.
    pub next_notification: Option<Ms>,
    pub notified_state: u8,
    pub notified: bool,
    pub notification_id: String,
    pub forced: bool,
    pub ack: Option<AckState>,
    /// Settings that commands can change at run time, and what the
    /// configuration said when they were saved. A value saved is put back
    /// only where the configuration still says what it said then: a
    /// changed configuration wins.
    pub notifications_enabled: bool,
    pub active_checks_enabled: bool,
    pub config_notifications_enabled: bool,
    pub config_active_checks_enabled: bool,
    /// The other settings that commands changed, by name, with what the
    /// configuration said when they were saved: only those that differ
    /// from the configuration, and `modified`, the bits of what commands
    /// changed.
    pub settings: Vec<Setting>,
}

/// A setting a command changed: its name (`passive_checks_enabled`,
/// `check_interval`, `_VARIABLE` for a custom variable ...), its value,
/// and what the configuration said when it was saved.
#[derive(Clone, Debug, PartialEq)]
pub struct Setting {
    pub name: String,
    pub value: String,
    pub configured: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AckState {
    pub author: String,
    pub comment: String,
    pub sticky: bool,
    pub persistent: bool,
    pub comment_id: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommentState {
    pub id: u64,
    pub host: String,
    pub service: Option<String>,
    pub entry_type: u8,
    pub persistent: bool,
    /// Seconds since the epoch.
    pub entry: i64,
    pub author: String,
    pub text: String,
    pub expires: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DowntimeState {
    pub id: u64,
    pub host: String,
    pub service: Option<String>,
    /// Seconds since the epoch.
    pub start: i64,
    pub end: i64,
    pub fixed: bool,
    pub trigger: u64,
    pub duration: i64,
    pub author: String,
    pub comment: String,
    pub entry: i64,
    pub triggered: Option<i64>,
    pub active: bool,
    pub comment_id: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub objects: Vec<ObjState>,
    pub comments: Vec<CommentState>,
    pub downtimes: Vec<DowntimeState>,
    pub next_comment: u64,
    pub next_downtime: u64,
    pub notifications_enabled: bool,
    /// The program-wide switches that commands changed, as for an
    /// object's settings.
    pub program: Vec<Setting>,
}

/// An object's settings that commands can change, other than
/// notifications and active checks, which have fields of their own.
fn object_settings(cfg: &Checkable, modified: u32) -> Vec<(String, String)> {
    let flag = |b: bool| if b { "1" } else { "0" }.to_string();
    let mut out = vec![
        ("passive_checks_enabled".to_string(), flag(cfg.passive_checks_enabled)),
        ("event_handler_enabled".to_string(), flag(cfg.event_handler_enabled)),
        ("flap_detection_enabled".to_string(), flag(cfg.flap_detection_enabled)),
        ("check_interval".to_string(), cfg.check_interval.to_string()),
        ("retry_interval".to_string(), cfg.retry_interval.to_string()),
        ("max_check_attempts".to_string(), cfg.max_check_attempts.to_string()),
        ("modified".to_string(), modified.to_string()),
    ];
    out.extend(cfg.custom.iter().map(|(name, value)| (format!("_{name}"), value.clone())));
    out
}

fn put_setting(cfg: &mut Checkable, modified: &mut u32, name: &str, value: &str) {
    let flag = value == "1";
    match name {
        "passive_checks_enabled" => cfg.passive_checks_enabled = flag,
        "event_handler_enabled" => cfg.event_handler_enabled = flag,
        "flap_detection_enabled" => cfg.flap_detection_enabled = flag,
        "check_interval" => cfg.check_interval = value.parse().unwrap_or(cfg.check_interval),
        "retry_interval" => cfg.retry_interval = value.parse().unwrap_or(cfg.retry_interval),
        "max_check_attempts" => cfg.max_check_attempts = value.parse::<u32>().unwrap_or(cfg.max_check_attempts).max(1),
        "modified" => *modified = value.parse().unwrap_or(0),
        _ => {
            if let Some(var) = name.strip_prefix('_') {
                if let Some(entry) = cfg.custom.iter_mut().find(|(n, _)| n == var) {
                    entry.1 = value.to_string();
                }
            }
        }
    }
}

/// The program-wide switches that commands can change.
fn program_settings(s: &Settings) -> Vec<(String, String)> {
    let flag = |b: bool| if b { "1" } else { "0" }.to_string();
    vec![
        ("execute_service_checks".to_string(), flag(s.execute_service_checks)),
        ("execute_host_checks".to_string(), flag(s.execute_host_checks)),
        ("accept_passive_service_checks".to_string(), flag(s.accept_passive_service_checks)),
        ("accept_passive_host_checks".to_string(), flag(s.accept_passive_host_checks)),
        ("enable_event_handlers".to_string(), flag(s.enable_event_handlers)),
        ("enable_flap_detection".to_string(), flag(s.enable_flap_detection)),
    ]
}

fn put_program(s: &mut Settings, name: &str, value: &str) {
    let on = value == "1";
    match name {
        "execute_service_checks" => s.execute_service_checks = on,
        "execute_host_checks" => s.execute_host_checks = on,
        "accept_passive_service_checks" => s.accept_passive_service_checks = on,
        "accept_passive_host_checks" => s.accept_passive_host_checks = on,
        "enable_event_handlers" => s.enable_event_handlers = on,
        "enable_flap_detection" => s.enable_flap_detection = on,
        _ => {}
    }
}

/// What differs between now and the configuration, with what the
/// configuration says.
fn changed(now: Vec<(String, String)>, configured: Vec<(String, String)>) -> Vec<Setting> {
    now.into_iter()
        .filter_map(|(name, value)| {
            let was = configured.iter().find(|(n, _)| *n == name).map(|(_, v)| v.clone()).unwrap_or_default();
            (value != was).then_some(Setting { name, value, configured: was })
        })
        .collect()
}

/// Puts back what was saved, where the configuration still says what it
/// said then.
fn still_configured<'a>(saved: &'a [Setting], configured: &[(String, String)]) -> impl Iterator<Item = &'a Setting> + use<'a> {
    let configured = configured.to_vec();
    saved.iter().filter(move |s| configured.iter().any(|(n, v)| *n == s.name && *v == s.configured))
}

impl Engine {
    /// An object's settings as the configuration gives them.
    fn configured_checkable(&self, obj: Obj) -> Option<&Checkable> {
        match obj {
            Obj::Host(at) => self.objects.hosts.get(&self.hosts[at].name).map(|h| &h.check),
            Obj::Service(at) => {
                let entry = &self.services[at];
                self.objects.services.get(&(entry.host_name.clone(), entry.description.clone())).map(|s| &s.check)
            }
        }
    }

    fn names(&self, obj: Obj) -> (String, Option<String>) {
        match obj {
            Obj::Host(at) => (self.hosts[at].name.clone(), None),
            Obj::Service(at) => (
                self.services[at].host_name.clone(),
                Some(self.services[at].description.clone()),
            ),
        }
    }

    fn named(&self, host: &str, service: &Option<String>) -> Option<Obj> {
        match service {
            None => self.host(host),
            Some(description) => self.service(host, description),
        }
    }

    pub fn all_objects(&self) -> Vec<Obj> {
        (0..self.hosts.len())
            .map(Obj::Host)
            .chain((0..self.services.len()).map(Obj::Service))
            .collect()
    }

    /// The object's settings as the configuration has them.
    fn configured(&self, obj: Obj) -> (bool, bool) {
        let cfg = match obj {
            Obj::Host(at) => self.objects.hosts.get(&self.hosts[at].name).map(|h| &h.check),
            Obj::Service(at) => {
                let entry = &self.services[at];
                self.objects
                    .services
                    .get(&(entry.host_name.clone(), entry.description.clone()))
                    .map(|s| &s.check)
            }
        };
        cfg.map_or((true, true), |cfg| (cfg.notifications_enabled, cfg.active_checks_enabled))
    }

    /// The state of one host or service.
    pub fn obj_state(&self, obj: Obj) -> ObjState {
        let item = self.item(obj);
        let (host, service) = self.names(obj);
        let (config_notifications, config_checks) = self.configured(obj);
        ObjState {
            host,
            service,
            state: item.state,
            last_state: item.last_state,
            last_hard_state: item.last_hard_state,
            hard: item.hard,
            attempt: item.attempt,
            checked: item.checked,
            output: item.output.short.clone(),
            long_output: item.output.long.clone(),
            perfdata: item.output.perfdata.clone(),
            last_check: item.last_check,
            last_state_change: item.last_state_change,
            last_hard_state_change: item.last_hard_state_change,
            last_time: item.last_time,
            flapping: item.flapping,
            percent_change: item.percent_change,
            history: item.history.states(),
            event_id: item.event_id,
            last_event_id: item.last_event_id,
            problem_id: item.problem_id.clone(),
            last_problem_id: item.last_problem_id.clone(),
            notification_number: item.notice.number,
            last_notification: item.notice.last,
            next_notification: item.notice.next,
            notified_state: item.notice.last_state,
            notified: item.notice.sent,
            notification_id: item.notice.id.clone(),
            forced: item.notice.forced,
            ack: item.notice.ack.as_ref().map(|ack| AckState {
                author: ack.author.clone(),
                comment: ack.comment.clone(),
                sticky: ack.sticky,
                persistent: ack.persistent,
                comment_id: ack.comment_id,
            }),
            notifications_enabled: item.cfg.notifications_enabled,
            active_checks_enabled: item.cfg.active_checks_enabled,
            config_notifications_enabled: config_notifications,
            config_active_checks_enabled: config_checks,
            settings: match self.configured_checkable(obj) {
                Some(cfg) => changed(object_settings(&item.cfg, item.modified), object_settings(cfg, 0)),
                None => Vec::new(),
            },
        }
    }

    pub fn comment_state(&self, comment: &Comment) -> CommentState {
        let (host, service) = self.names(comment.obj);
        CommentState {
            id: comment.id,
            host,
            service,
            entry_type: comment.entry_type,
            persistent: comment.persistent,
            entry: comment.entry,
            author: comment.author.clone(),
            text: comment.text.clone(),
            expires: comment.expires,
        }
    }

    pub fn downtime_state(&self, downtime: &Downtime) -> DowntimeState {
        let (host, service) = self.names(downtime.obj);
        DowntimeState {
            id: downtime.id,
            host,
            service,
            start: downtime.start,
            end: downtime.end,
            fixed: downtime.fixed,
            trigger: downtime.trigger,
            duration: downtime.duration,
            author: downtime.author.clone(),
            comment: downtime.comment.clone(),
            entry: downtime.entry,
            triggered: downtime.triggered,
            active: downtime.active,
            comment_id: downtime.comment_id,
        }
    }

    /// All of the engine's state that a restart should keep.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            objects: self.all_objects().into_iter().map(|obj| self.obj_state(obj)).collect(),
            comments: self.comments.values().map(|c| self.comment_state(c)).collect(),
            downtimes: self.downtimes.values().map(|d| self.downtime_state(d)).collect(),
            next_comment: self.next_comment,
            next_downtime: self.next_downtime,
            notifications_enabled: self.settings.enable_notifications,
            program: changed(program_settings(&self.settings), program_settings(&self.configured_settings)),
        }
    }

    /// Puts saved state back. What names no object of this configuration
    /// is dropped; the answer says how much.
    pub fn restore(&mut self, snapshot: &Snapshot) -> usize {
        let mut dropped = 0;
        for saved in &snapshot.objects {
            let Some(obj) = self.named(&saved.host, &saved.service) else {
                dropped += 1;
                continue;
            };
            let (config_notifications, config_checks) = self.configured(obj);
            let item = self.item_mut(obj);
            item.state = saved.state.min(if matches!(obj, Obj::Host(_)) { 2 } else { 3 });
            item.last_state = saved.last_state;
            item.last_hard_state = saved.last_hard_state;
            item.hard = saved.hard;
            item.attempt = saved.attempt.max(1);
            item.checked = saved.checked;
            item.output = Output {
                short: saved.output.clone(),
                long: saved.long_output.clone(),
                perfdata: saved.perfdata.clone(),
            };
            item.last_check = saved.last_check;
            item.last_state_change = saved.last_state_change;
            item.last_hard_state_change = saved.last_hard_state_change;
            item.last_time = saved.last_time;
            item.flapping = saved.flapping;
            item.percent_change = saved.percent_change;
            if !saved.history.is_empty() {
                item.history = History::from_states(&saved.history);
            }
            item.event_id = saved.event_id;
            item.last_event_id = saved.last_event_id;
            item.problem_id = saved.problem_id.clone();
            item.last_problem_id = saved.last_problem_id.clone();
            item.notice.number = saved.notification_number;
            item.notice.last = saved.last_notification;
            item.notice.next = saved.next_notification;
            item.notice.last_state = saved.notified_state;
            item.notice.sent = saved.notified;
            item.notice.id = saved.notification_id.clone();
            item.notice.forced = saved.forced;
            item.notice.ack = saved.ack.as_ref().map(|ack| Ack {
                author: ack.author.clone(),
                comment: ack.comment.clone(),
                sticky: ack.sticky,
                persistent: ack.persistent,
                comment_id: ack.comment_id,
            });
            item.notice.downtime_depth = 0;
            if saved.config_notifications_enabled == config_notifications {
                item.cfg.notifications_enabled = saved.notifications_enabled;
            }
            if saved.config_active_checks_enabled == config_checks {
                item.cfg.active_checks_enabled = saved.active_checks_enabled;
            }
            let configured = self.configured_checkable(obj).map(|cfg| object_settings(cfg, 0)).unwrap_or_default();
            let item = self.item_mut(obj);
            for setting in still_configured(&saved.settings, &configured) {
                put_setting(&mut item.cfg, &mut item.modified, &setting.name, &setting.value);
            }
        }
        self.comments.clear();
        for saved in &snapshot.comments {
            let Some(obj) = self.named(&saved.host, &saved.service) else {
                dropped += 1;
                continue;
            };
            self.comments.insert(
                saved.id,
                Comment {
                    id: saved.id,
                    obj,
                    entry_type: saved.entry_type,
                    persistent: saved.persistent,
                    entry: saved.entry,
                    author: saved.author.clone(),
                    text: saved.text.clone(),
                    expires: saved.expires,
                },
            );
        }
        self.downtimes.clear();
        self.timers.clear();
        for saved in &snapshot.downtimes {
            let Some(obj) = self.named(&saved.host, &saved.service) else {
                dropped += 1;
                continue;
            };
            let downtime = Downtime {
                id: saved.id,
                obj,
                start: saved.start,
                end: saved.end,
                fixed: saved.fixed,
                trigger: saved.trigger,
                duration: saved.duration,
                author: saved.author.clone(),
                comment: saved.comment.clone(),
                entry: saved.entry,
                triggered: saved.triggered,
                active: saved.active,
                comment_id: saved.comment_id,
            };
            if downtime.active {
                self.item_mut(obj).notice.downtime_depth += 1;
                let end = match (downtime.fixed, downtime.triggered) {
                    (false, Some(at)) => at + downtime.duration,
                    _ => downtime.end,
                };
                self.add_timer(end * 1000, Timer::DowntimeEnd(downtime.id));
            } else if downtime.fixed {
                if downtime.trigger == 0 {
                    self.add_timer(downtime.start * 1000, Timer::DowntimeStart(downtime.id));
                }
            } else {
                self.add_timer((downtime.end + 1) * 1000, Timer::DowntimeExpire(downtime.id));
            }
            self.downtimes.insert(saved.id, downtime);
        }
        let highest_comment = self.comments.keys().max().copied().unwrap_or(0);
        let highest_downtime = self.downtimes.keys().max().copied().unwrap_or(0);
        self.next_comment = snapshot.next_comment.max(highest_comment);
        self.next_downtime = snapshot.next_downtime.max(highest_downtime);
        self.settings.enable_notifications = snapshot.notifications_enabled;
        let configured = program_settings(&self.configured_settings);
        for setting in still_configured(&snapshot.program, &configured) {
            put_program(&mut self.settings, &setting.name, &setting.value);
        }
        dropped
    }
}
