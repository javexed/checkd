//! Notifications, acknowledgements, downtimes and comments.
//!
//! The rules are the reference version's, read off its log and the calls
//! of scripted notification commands on configs written for the purpose.
//!
//! # When a notification is tried
//!
//! - At every hard result of a host or service that is not OK, and at
//!   the hard result that ends such a problem (a recovery). Soft results
//!   are not notified.
//! - For a volatile service, at every hard result that is not OK.
//! - When an acknowledgement is made with its notify flag, when flapping
//!   starts or stops, and when a downtime starts, ends or is cancelled.
//! - For a host that is not up, also at every result of a service on it
//!   that stays in the same hard problem: the host's notification is
//!   tried first, then the service's.
//!
//! # What stops a problem or recovery notification
//!
//! In this order, each with its own line in the log:
//!
//! 1. notifications turned off for the object;
//! 2. the object in a downtime;
//! 3. a soft state;
//! 4. an acknowledged problem;
//! 5. a state that the object's `notification_options` leave out;
//! 6. a recovery when no notification went out for the problem;
//! 7. flapping;
//! 8. the object's notification period;
//! 9. for a service, a host that is not up;
//! 10. `first_notification_delay`, counted from the change to the hard
//!     state;
//! 11. a re-notification before `notification_interval` has passed, or
//!     at all when the interval is 0. A change from one problem state to
//!     another is notified whatever the interval.
//!
//! Then each contact on its own: its own options and its notification
//! period. When no contact is left, that is logged. A line saying why a
//! notification was stopped is written only when the reason is not the
//! one last written for that object, or for that contact.
//!
//! The notification number counts the problem notifications that went
//! out, and the recovery after them. It starts again after a recovery.
//! Escalations are looked up by that number; a recovery is sent to whom
//! the last problem notification would have gone.

use std::collections::BTreeMap;

use checkd_config::model::Contact;
use checkd_config::values::Opts;

use crate::engine::{About, Effect, Engine, JobKind, Ms, Obj, HOST_STATES, SERVICE_STATES};

/// What a notification is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A problem, or a recovery.
    Normal,
    Acknowledgement,
    FlappingStart,
    FlappingStop,
    FlappingDisabled,
    DowntimeStart,
    DowntimeEnd,
    DowntimeCancelled,
    Custom,
}

impl Kind {
    fn name(self, recovery: bool) -> &'static str {
        match self {
            Kind::Normal if recovery => "RECOVERY",
            Kind::Normal => "PROBLEM",
            Kind::Acknowledgement => "ACKNOWLEDGEMENT",
            Kind::FlappingStart => "FLAPPINGSTART",
            Kind::FlappingStop => "FLAPPINGSTOP",
            Kind::FlappingDisabled => "FLAPPINGDISABLED",
            Kind::DowntimeStart => "DOWNTIMESTART",
            Kind::DowntimeEnd => "DOWNTIMEEND",
            Kind::DowntimeCancelled => "DOWNTIMECANCELLED",
            Kind::Custom => "CUSTOM",
        }
    }

    /// The letter of `notification_options` that lets such a
    /// notification through, where one is needed.
    fn letter(self) -> Option<char> {
        match self {
            Kind::FlappingStart | Kind::FlappingStop | Kind::FlappingDisabled => Some('f'),
            Kind::DowntimeStart | Kind::DowntimeEnd | Kind::DowntimeCancelled => Some('s'),
            _ => None,
        }
    }
}

/// What the macros of a notification command see.
#[derive(Clone, Debug, Default)]
pub struct Notice {
    pub kind: String,
    pub contact: String,
    pub recipients: String,
    pub escalated: bool,
    pub author: String,
    pub comment: String,
    pub id: String,
}

/// An acknowledgement of a problem.
#[derive(Clone, Debug)]
pub struct Ack {
    pub author: String,
    pub comment: String,
    /// Kept until the object is OK, not only until its state changes.
    pub sticky: bool,
    pub comment_id: u64,
    pub persistent: bool,
}

/// What the engine keeps about notifications for one host or service.
#[derive(Clone, Debug, Default)]
pub struct NoticeState {
    /// Notifications that went out for the problem, and its recovery.
    pub number: u32,
    /// When the last problem notification went out.
    pub last: Ms,
    /// When the next may go out. `None` when no more may.
    pub next: Option<Ms>,
    /// The state the last problem notification was about.
    pub last_state: u8,
    /// Whether a problem notification went out for this problem.
    pub sent: bool,
    /// The id of the last notification.
    pub id: String,
    /// The reason last written to the log for a notification stopped.
    pub reason: Option<&'static str>,
    pub ack: Option<Ack>,
    /// How many downtimes the object is in.
    pub downtime_depth: u32,
    /// A service that went to its hard state only because its host was
    /// not up, before its own attempts were used up. Such a problem is
    /// not notified, at that result or at the ones after it, until the
    /// state changes.
    pub forced: bool,
}

pub struct ContactEntry {
    pub cfg: Contact,
    /// The reason last written for a host notification, and for a service
    /// notification, that was stopped for this contact.
    pub reason: [Option<&'static str>; 2],
}

/// A downtime of a host or service.
#[derive(Clone, Debug)]
pub struct Downtime {
    pub id: u64,
    pub obj: Obj,
    /// Seconds since the epoch.
    pub start: i64,
    pub end: i64,
    pub fixed: bool,
    pub trigger: u64,
    /// Seconds, for a flexible downtime.
    pub duration: i64,
    pub author: String,
    pub comment: String,
    pub entry: i64,
    /// For a flexible downtime: when it was set off.
    pub triggered: Option<i64>,
    pub active: bool,
    pub comment_id: u64,
}

#[derive(Clone, Debug)]
pub struct Comment {
    pub id: u64,
    pub obj: Obj,
    /// 1 a comment of a user, 2 of a downtime, 3 of flapping, 4 of an
    /// acknowledgement.
    pub entry_type: u8,
    pub persistent: bool,
    pub entry: i64,
    pub author: String,
    pub text: String,
    pub expires: Option<i64>,
}

/// Something that is due at a time and is not a check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Timer {
    DowntimeStart(u64),
    DowntimeEnd(u64),
    /// A flexible downtime that was not set off in its window.
    DowntimeExpire(u64),
    /// The processing command of the host (0) or service (1) performance
    /// data file.
    PerfdataProcessing(u8),
    /// A look at the freshness of the hosts (0) or the services (1).
    Freshness(u8),
}

const REASON_OFF: &str = "Notifications are disabled for this object by its configuration.";
const REASON_OFF_BY_COMMAND: &str =
    "Notifications are temporarily disabled for this object by an external command.";
const REASON_DOWNTIME: &str = "Notification blocked for object currently in a scheduled downtime.";
const REASON_SOFT: &str = "Notifications blocked for object in a soft state.";
const REASON_ACKED: &str = "Notification for problem blocked because it has already been acknowledged.";
const REASON_STATE: &str = "Notifications disabled for current object state.";
const REASON_RECOVERY: &str =
    "Notification blocked for RECOVERY because no notification was sent out for the original problem.";
const REASON_FLAPPING: &str = "Notification blocked because the object is currently flapping.";
const REASON_PERIOD: &str =
    "Notification blocked by timeperiod; notifications should not be sent out at this time.";
const REASON_HOST: &str =
    "Notification blocked for service because its associated host is either down or unreachable.";
const REASON_DEPENDENCY: &str = "Notification blocked due to dependency of another object.";
const REASON_HOST_DOWNTIME: &str =
    "Notification blocked for service because its associated host is currently in a scheduled downtime.";
const REASON_DELAY: &str = "Notification blocked because first_notification_delay is configured and not enough time has elapsed since the object changed to a non-UP/non-OK state (or since program start).";
const REASON_ONCE: &str = "Re-notification blocked for this problem.";
const REASON_TOO_SOON: &str =
    "Re-notification blocked for this problem because not enough time has passed since last notification.";
const REASON_NOBODY: &str = "No notification sent, because no contacts were found for notification purposes.";

const HOST_LETTERS: [char; 3] = ['o', 'd', 'u'];
const SERVICE_LETTERS: [char; 4] = ['o', 'w', 'c', 'u'];

impl Engine {
    /// Tries a notification about an object, and sends it to each contact
    /// that passes.
    pub(crate) fn notify(
        &mut self,
        obj: Obj,
        kind: Kind,
        author: &str,
        comment: &str,
        now: Ms,
        out: &mut Vec<Effect>,
    ) {
        if !self.settings.enable_notifications {
            return;
        }
        let item = self.item(obj);
        let state = item.state;
        let recovery = kind == Kind::Normal && state == 0;
        if let Err(reason) = self.may_notify(obj, kind, now, out) {
            self.suppressed(obj, reason, out);
            if recovery {
                self.recovered(obj);
            }
            return;
        }

        // Who it goes to, and how often it may be sent again.
        let item = self.item(obj);
        let number = match kind {
            Kind::Normal if recovery => item.notice.number,
            Kind::Normal => item.notice.number + 1,
            _ => item.notice.number,
        };
        let (people, escalated, interval) = self.recipients(obj, number, state, now);

        let mut chosen: Vec<usize> = Vec::new();
        for at in people {
            match self.contact_may(obj, at, kind, state, now) {
                Ok(()) => chosen.push(at),
                Err(reason) => self.contact_suppressed(obj, at, reason, out),
            }
        }
        if chosen.is_empty() {
            self.suppressed(obj, REASON_NOBODY, out);
            if recovery {
                self.recovered(obj);
            }
            return;
        }

        let id = self.new_id();
        if kind == Kind::Normal {
            let item = self.item_mut(obj);
            item.notice.id = id.clone();
            if recovery {
                item.notice.number += 1;
            } else {
                item.notice.number = number;
                item.notice.sent = true;
                item.notice.last = now;
                item.notice.last_state = state;
                // The reference counts in whole seconds.
                item.notice.next = interval.map(|ms| (now.div_euclid(1000) * 1000) + ms);
            }
        }

        let names: Vec<String> = chosen.iter().map(|&at| self.contacts[at].cfg.name.clone()).collect();
        let recipients = names.join(",");
        let state_name = self.state_name(obj);
        let shown = match kind {
            Kind::Normal => state_name.to_string(),
            _ => format!("{} ({state_name})", kind.name(false)),
        };
        let with_text = !matches!(kind, Kind::Normal);
        for &at in chosen.iter().rev() {
            let contact = self.contacts[at].cfg.clone();
            let commands = match obj {
                Obj::Host(_) => &contact.host_notification_commands,
                Obj::Service(_) => &contact.service_notification_commands,
            };
            let notice = Notice {
                kind: kind.name(recovery).to_string(),
                contact: contact.name.clone(),
                recipients: recipients.clone(),
                escalated,
                author: author.to_string(),
                comment: comment.to_string(),
                id: id.clone(),
            };
            for command in commands {
                let Some(line) = self.notice_line(command, obj, &notice, now) else {
                    continue;
                };
                let output = self.item(obj).output.short.clone();
                let label = match obj {
                    Obj::Host(_) => "HOST NOTIFICATION",
                    Obj::Service(_) => "SERVICE NOTIFICATION",
                };
                let mut text = format!(
                    "{label}: {};{};{shown};{command};{output}",
                    contact.name,
                    self.log_name(obj)
                );
                if with_text {
                    text.push_str(&format!(";{author};{comment}"));
                }
                if self.settings.log_notifications {
                    out.push(Effect::Log(text));
                }
                let timeout = self.settings.notification_timeout as Ms * 1000;
                self.run_job(obj, JobKind::Notification, line, timeout, now, out);
                self.last_job_notice(&contact.name, command);
                if let Some(Effect::Run(job)) = out.last_mut() {
                    job.about = Some(Box::new(About {
                        contact: contact.name.clone(),
                        kind: notice.kind.clone(),
                        what: self.log_name(obj).replace(';', "/"),
                        state: shown.clone(),
                        output,
                    }));
                }
            }
        }
        if recovery {
            self.recovered(obj);
        }
    }

    /// After a recovery, notified or not: the problem is over.
    fn recovered(&mut self, obj: Obj) {
        let notice = &mut self.item_mut(obj).notice;
        notice.number = 0;
        notice.sent = false;
        notice.next = Some(0);
    }

    /// The checks a notification goes through before its contacts are
    /// looked at. `Err` holds the reason it stops.
    fn may_notify(&mut self, obj: Obj, kind: Kind, now: Ms, out: &mut Vec<Effect>) -> Result<(), &'static str> {
        let item = self.item(obj);
        let cfg = &item.cfg;
        let state = item.state;
        let letters: &[char] = match obj {
            Obj::Host(_) => &HOST_LETTERS,
            Obj::Service(_) => &SERVICE_LETTERS,
        };
        if !cfg.notifications_enabled {
            if item.modified & crate::engine::MOD_NOTIFICATIONS != 0 {
                return Err(REASON_OFF_BY_COMMAND);
            }
            return Err(REASON_OFF);
        }
        match kind {
            Kind::Normal => {}
            Kind::Acknowledgement | Kind::Custom => return Ok(()),
            _ => {
                let letter = kind.letter().unwrap_or('n');
                if !cfg.notification_options.has(letter) {
                    return Err(REASON_STATE);
                }
                // Flapping notifications are not sent during a downtime.
                if matches!(kind, Kind::FlappingStart | Kind::FlappingStop) && self.in_downtime(obj) {
                    return Err(REASON_DOWNTIME);
                }
                return Ok(());
            }
        }
        // A service is held back here by its own downtime. Its host's
        // downtime counts further on.
        // A recovery goes out in a downtime too, when the problem was
        // notified before it.
        if item.notice.downtime_depth > 0 && state != 0 {
            return Err(REASON_DOWNTIME);
        }
        if !item.hard {
            return Err(REASON_SOFT);
        }
        if state != 0 && item.notice.ack.is_some() {
            return Err(REASON_ACKED);
        }
        let letter = if state == 0 { 'r' } else { letters[state as usize] };
        if !cfg.notification_options.has(letter) {
            return Err(REASON_STATE);
        }
        if state == 0 && !item.notice.sent {
            return Err(REASON_RECOVERY);
        }
        if item.flapping {
            return Err(REASON_FLAPPING);
        }
        if let Some(period) = &cfg.notification_period {
            if !self.periods().is_valid(period, now.div_euclid(1000)) {
                return Err(REASON_PERIOD);
            }
        }
        if state == 0 {
            return Ok(());
        }
        // A notification dependency: seen in the reference for a problem.
        // Where it stands among the other reasons, and whether it holds
        // a recovery back, was not seen.
        if self.kept_quiet(obj, now.div_euclid(1000)) {
            return Err(REASON_DEPENDENCY);
        }
        if let Obj::Service(at) = obj {
            let host = self.services[at].host;
            if self.hosts[host].item.state != 0 {
                return Err(REASON_HOST);
            }
            if self.hosts[host].item.notice.downtime_depth > 0 {
                return Err(REASON_HOST_DOWNTIME);
            }
        }
        let item = self.item(obj);
        let delay = (item.cfg.first_notification_delay * self.settings.interval_length * 1000.0) as Ms;
        let second = |ms: Ms| ms.div_euclid(1000) * 1000;
        if !item.notice.sent && delay > 0 {
            let since = if item.last_hard_state_change > 0 { item.last_hard_state_change } else { self.start };
            if second(now) < second(since) + delay {
                return Err(REASON_DELAY);
            }
        }
        let volatile = matches!(obj, Obj::Service(at) if self.services[at].volatile);
        if item.notice.sent && item.notice.last_state == state && !volatile {
            match item.notice.next {
                None => return Err(REASON_ONCE),
                Some(next) if second(now) < next => return Err(REASON_TOO_SOON),
                _ => {}
            }
        }
        let _ = out;
        Ok(())
    }

    /// The contacts a notification with this number goes to, whether an
    /// escalation chose them, and the interval before the next one: `None`
    /// when no other may follow.
    fn recipients(&self, obj: Obj, number: u32, state: u8, now: Ms) -> (Vec<usize>, bool, Option<Ms>) {
        let item = self.item(obj);
        let unit = self.settings.interval_length * 1000.0;
        let own_interval = item.cfg.notification_interval;
        // A recovery goes to whom the last problem notification went.
        let wanted = if state == 0 { number.max(1) } else { number };
        let letters: &[char] = match obj {
            Obj::Host(_) => &HOST_LETTERS,
            Obj::Service(_) => &SERVICE_LETTERS,
        };
        let letter = if state == 0 { 'r' } else { letters[state as usize] };
        let second = now.div_euclid(1000);
        let mut people: Vec<usize> = Vec::new();
        let mut escalated = false;
        let mut interval: Option<f64> = None;
        let escalations: Vec<EscalationView> = match obj {
            Obj::Host(at) => self.host_escalations[at]
                .iter()
                .map(|&e| EscalationView::of(&self.objects.host_escalations[e]))
                .collect(),
            Obj::Service(at) => self.service_escalations[at]
                .iter()
                .map(|&e| EscalationView::of(&self.objects.service_escalations[e]))
                .collect(),
        };
        for escalation in escalations {
            let first = escalation.first_notification.unwrap_or(1);
            let last = escalation.last_notification.unwrap_or(0);
            if wanted < first || (last > 0 && wanted > last) {
                continue;
            }
            if !escalation.options.is_none() && !escalation.options.has(letter) {
                continue;
            }
            if let Some(period) = &escalation.period {
                if !self.periods().is_valid(period, second) {
                    continue;
                }
            }
            escalated = true;
            self.add_people(&mut people, &escalation.contacts, &escalation.contact_groups);
            let this = escalation.notification_interval;
            interval = Some(match interval {
                Some(other) => other.min(this),
                None => this,
            });
        }
        if !escalated {
            self.add_people(&mut people, &item.cfg.contacts, &item.cfg.contact_groups);
            interval = Some(own_interval);
        }
        let interval = interval.unwrap_or(own_interval);
        let ms = if interval <= 0.0 { None } else { Some((interval * unit).round() as Ms) };
        (people, escalated, ms)
    }

    /// The contacts named, the last first, then the members of the
    /// groups, each once. The reference's `$NOTIFICATIONRECIPIENTS$` show
    /// that order.
    fn add_people(&self, people: &mut Vec<usize>, contacts: &[String], groups: &[String]) {
        for name in contacts.iter().rev() {
            if let Some(&at) = self.contact_index.get(name) {
                if !people.contains(&at) {
                    people.push(at);
                }
            }
        }
        for group in groups {
            let Some(group) = self.objects.contactgroups.get(group) else {
                continue;
            };
            for name in &group.members {
                if let Some(&at) = self.contact_index.get(name) {
                    if !people.contains(&at) {
                        people.push(at);
                    }
                }
            }
        }
    }

    fn contact_may(&self, obj: Obj, at: usize, kind: Kind, state: u8, now: Ms) -> Result<(), &'static str> {
        let contact = &self.contacts[at].cfg;
        let (on, options, period, letters): (bool, &Opts, &Option<String>, &[char]) = match obj {
            Obj::Host(_) => (
                contact.host_notifications_enabled,
                &contact.host_notification_options,
                &contact.host_notification_period,
                &HOST_LETTERS,
            ),
            Obj::Service(_) => (
                contact.service_notifications_enabled,
                &contact.service_notification_options,
                &contact.service_notification_period,
                &SERVICE_LETTERS,
            ),
        };
        if !on {
            return Err(REASON_OFF);
        }
        let letter = match kind.letter() {
            Some(letter) => letter,
            None if state == 0 => 'r',
            None => letters[state as usize],
        };
        if !matches!(kind, Kind::Custom) && !options.has(letter) {
            return Err(REASON_STATE);
        }
        if let Some(period) = period {
            if !self.periods().is_valid(period, now.div_euclid(1000)) {
                return Err(REASON_PERIOD);
            }
        }
        Ok(())
    }

    fn suppressed(&mut self, obj: Obj, reason: &'static str, out: &mut Vec<Effect>) {
        let item = self.item_mut(obj);
        if item.notice.reason == Some(reason) {
            return;
        }
        item.notice.reason = Some(reason);
        let label = match obj {
            Obj::Host(_) => "HOST NOTIFICATION SUPPRESSED",
            Obj::Service(_) => "SERVICE NOTIFICATION SUPPRESSED",
        };
        out.push(Effect::Log(format!("{label}: {};{reason}", self.log_name(obj))));
    }

    fn contact_suppressed(&mut self, obj: Obj, at: usize, reason: &'static str, out: &mut Vec<Effect>) {
        let slot = matches!(obj, Obj::Service(_)) as usize;
        if self.contacts[at].reason[slot] == Some(reason) {
            return;
        }
        self.contacts[at].reason[slot] = Some(reason);
        let label = match obj {
            Obj::Host(_) => "HOST CONTACT NOTIFICATION SUPPRESSED",
            Obj::Service(_) => "SERVICE CONTACT NOTIFICATION SUPPRESSED",
        };
        let name = self.contacts[at].cfg.name.clone();
        out.push(Effect::Log(format!("{label}: {name};{};{reason}", self.log_name(obj))));
    }

    /// Whether the object is in a downtime, or is a service whose host is.
    pub fn in_downtime(&self, obj: Obj) -> bool {
        match obj {
            Obj::Host(at) => self.hosts[at].item.notice.downtime_depth > 0,
            Obj::Service(at) => {
                self.services[at].item.notice.downtime_depth > 0
                    || self.hosts[self.services[at].host].item.notice.downtime_depth > 0
            }
        }
    }

    /// What follows a result for notifications. `changed` says that the
    /// state, or the state type, is not what it was.
    pub(crate) fn after_result(&mut self, obj: Obj, was: u8, changed: bool, now: Ms, out: &mut Vec<Effect>) {
        let item = self.item(obj);
        let state = item.state;
        let hard = item.hard;

        // An acknowledgement ends with the problem, or, when it is not
        // sticky, with any change of state.
        if let Some(ack) = &item.notice.ack {
            if state == 0 || (!ack.sticky && state != was) {
                self.remove_ack(obj);
            }
        }

        // A flexible downtime is set off by a hard problem. The result
        // that sets it off is not notified.
        if state != 0 && hard && self.trigger_flexible(obj, now, out) {
            return;
        }

        if !hard {
            return;
        }
        if state == 0 && was == 0 {
            return;
        }
        // A service that stays in a hard problem while its host is not up:
        // the host's notification is tried first.
        if let Obj::Service(at) = obj {
            let host = self.services[at].host;
            if state != 0 && !changed && self.hosts[host].item.state != 0 {
                self.notify(Obj::Host(host), Kind::Normal, "", "", now, out);
            }
        }
        if state != 0 && self.item(obj).notice.forced {
            return;
        }
        self.notify(obj, Kind::Normal, "", "", now, out);
    }

    pub(crate) fn remove_ack(&mut self, obj: Obj) {
        if let Some(ack) = self.item_mut(obj).notice.ack.take() {
            if !ack.persistent {
                self.comments.remove(&ack.comment_id);
            }
        }
    }

    /// Acknowledges an object's problem. False when it has none to
    /// acknowledge.
    pub(crate) fn acknowledge(
        &mut self,
        obj: Obj,
        sticky: bool,
        notify: bool,
        persistent: bool,
        author: &str,
        comment: &str,
        now: Ms,
        out: &mut Vec<Effect>,
    ) -> bool {
        let item = self.item(obj);
        if item.state == 0 || !item.hard {
            return false;
        }
        let comment_id = self.add_comment(obj, 4, persistent, author, comment, now, None);
        if let Some(old) = self.item_mut(obj).notice.ack.take() {
            if !old.persistent {
                self.comments.remove(&old.comment_id);
            }
        }
        self.item_mut(obj).notice.ack = Some(Ack {
            author: author.to_string(),
            comment: comment.to_string(),
            sticky,
            comment_id,
            persistent,
        });
        if notify {
            self.notify(obj, Kind::Acknowledgement, author, comment, now, out);
        }
        true
    }

    pub(crate) fn add_comment(
        &mut self,
        obj: Obj,
        entry_type: u8,
        persistent: bool,
        author: &str,
        text: &str,
        now: Ms,
        expires: Option<i64>,
    ) -> u64 {
        self.next_comment += 1;
        let id = self.next_comment;
        self.comments.insert(
            id,
            Comment {
                id,
                obj,
                entry_type,
                persistent,
                entry: now.div_euclid(1000),
                author: author.to_string(),
                text: text.to_string(),
                expires,
            },
        );
        id
    }

    /// Schedules a downtime. `Err` when it cannot be: its end is past, or
    /// it ends before it starts.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn schedule_downtime(
        &mut self,
        obj: Obj,
        start: i64,
        end: i64,
        fixed: bool,
        trigger: u64,
        duration: i64,
        author: &str,
        comment: &str,
        now: Ms,
        out: &mut Vec<Effect>,
    ) -> Result<u64, ()> {
        let second = now.div_euclid(1000);
        if end < start || end <= second {
            return Err(());
        }
        self.next_downtime += 1;
        let id = self.next_downtime;
        // The comment that says so is made now, in the reference's words.
        let what = match obj {
            Obj::Host(_) => "host",
            Obj::Service(_) => "service",
        };
        let text = if fixed {
            format!(
                "This {what} has been scheduled for fixed downtime from {} to {}.  Notifications for the {what} will not be sent out during that time period.",
                self.short_datetime(start),
                self.short_datetime(end)
            )
        } else {
            format!(
                "This {what} has been scheduled for flexible downtime starting between {} and {} and lasting for a period of {} hours and {} minutes.  Notifications for the {what} will not be sent out during that time period.",
                self.short_datetime(start),
                self.short_datetime(end),
                duration / 3600,
                duration % 3600 / 60
            )
        };
        let comment_id = self.add_comment(obj, 2, false, author, &text, now, None);
        self.downtimes.insert(
            id,
            Downtime {
                id,
                obj,
                start,
                end,
                fixed,
                trigger,
                duration,
                author: author.to_string(),
                comment: comment.to_string(),
                entry: second,
                triggered: None,
                active: false,
                comment_id,
            },
        );
        if fixed {
            if trigger == 0 {
                self.add_timer(start * 1000, Timer::DowntimeStart(id));
            }
        } else {
            // A flexible downtime waits for a problem in its window.
            self.add_timer((end + 1) * 1000, Timer::DowntimeExpire(id));
            if self.item(obj).state != 0 && self.item(obj).hard && start <= second {
                self.trigger_flexible(obj, now, out);
            }
        }
        if fixed && trigger == 0 && start <= second {
            self.run_timers(now, out);
        }
        Ok(id)
    }

    /// Sets off the flexible downtimes of an object that are waiting.
    /// True when there was one.
    fn trigger_flexible(&mut self, obj: Obj, now: Ms, out: &mut Vec<Effect>) -> bool {
        let second = now.div_euclid(1000);
        let due: Vec<u64> = self
            .downtimes
            .values()
            .filter(|d| d.obj == obj && !d.fixed && d.triggered.is_none() && d.trigger == 0)
            .filter(|d| d.start <= second && second <= d.end)
            .map(|d| d.id)
            .collect();
        let any = !due.is_empty();
        for id in due {
            let downtime = self.downtimes.get_mut(&id).unwrap();
            downtime.triggered = Some(second);
            self.start_downtime(id, now, out);
        }
        any
    }

    fn start_downtime(&mut self, id: u64, now: Ms, out: &mut Vec<Effect>) {
        let Some(downtime) = self.downtimes.get(&id).cloned() else {
            return;
        };
        if downtime.active {
            return;
        }
        let second = now.div_euclid(1000);
        let obj = downtime.obj;
        let depth = self.item(obj).notice.downtime_depth;
        if depth == 0 {
            let (label, what) = match obj {
                Obj::Host(_) => ("HOST DOWNTIME ALERT", "Host has entered a period of scheduled downtime"),
                Obj::Service(_) => ("SERVICE DOWNTIME ALERT", "Service has entered a period of scheduled downtime"),
            };
            out.push(Effect::Log(format!("{label}: {};STARTED; {what}", self.log_name(obj))));
            self.notify(obj, Kind::DowntimeStart, &downtime.author, &downtime.comment, now, out);
        }
        let end = if downtime.fixed { downtime.end } else { second + downtime.duration };
        let entry = self.downtimes.get_mut(&id).unwrap();
        entry.active = true;
        self.item_mut(obj).notice.downtime_depth += 1;
        self.add_timer(end * 1000, Timer::DowntimeEnd(id));
        // Downtimes that wait for this one start with it.
        let triggered: Vec<u64> = self.downtimes.values().filter(|d| d.trigger == id).map(|d| d.id).collect();
        for other in triggered {
            self.start_downtime(other, now, out);
        }
    }

    /// Ends a downtime: at its end, or cancelled.
    pub(crate) fn stop_downtime(&mut self, id: u64, cancelled: bool, now: Ms, out: &mut Vec<Effect>) -> bool {
        let Some(downtime) = self.downtimes.remove(&id) else {
            return false;
        };
        let obj = downtime.obj;
        self.comments.remove(&downtime.comment_id);
        if downtime.active {
            let item = self.item_mut(obj);
            item.notice.downtime_depth = item.notice.downtime_depth.saturating_sub(1);
            if item.notice.downtime_depth == 0 {
                let (label, what) = match (obj, cancelled) {
                    (Obj::Host(_), false) => ("HOST DOWNTIME ALERT", "STOPPED; Host has exited from a period of scheduled downtime"),
                    (Obj::Host(_), true) => ("HOST DOWNTIME ALERT", "CANCELLED; Scheduled downtime for host has been cancelled."),
                    (Obj::Service(_), false) => {
                        ("SERVICE DOWNTIME ALERT", "STOPPED; Service has exited from a period of scheduled downtime")
                    }
                    (Obj::Service(_), true) => {
                        ("SERVICE DOWNTIME ALERT", "CANCELLED; Scheduled downtime for service has been cancelled.")
                    }
                };
                out.push(Effect::Log(format!("{label}: {};{what}", self.log_name(obj))));
                if cancelled {
                    self.notify(obj, Kind::DowntimeCancelled, "", "", now, out);
                } else {
                    self.notify(obj, Kind::DowntimeEnd, &downtime.author, &downtime.comment, now, out);
                }
            }
        } else if !cancelled && !downtime.fixed {
            // A flexible downtime that nothing set off: the reference
            // still says that it ended.
            self.notify(obj, Kind::DowntimeEnd, &downtime.author, &downtime.comment, now, out);
        }
        // Downtimes that were waiting for this one go with it.
        let waiting: Vec<u64> = self.downtimes.values().filter(|d| d.trigger == id).map(|d| d.id).collect();
        for other in waiting {
            self.stop_downtime(other, cancelled, now, out);
        }
        true
    }

    pub(crate) fn add_timer(&mut self, at: Ms, timer: Timer) {
        self.timer_seq += 1;
        self.timers.insert((at, self.timer_seq, timer));
    }

    /// Runs the timers that are due.
    pub(crate) fn run_timers(&mut self, now: Ms, out: &mut Vec<Effect>) {
        while let Some(&(at, seq, timer)) = self.timers.iter().next() {
            if at > now {
                break;
            }
            self.timers.remove(&(at, seq, timer));
            match timer {
                Timer::DowntimeStart(id) => self.start_downtime(id, now, out),
                Timer::DowntimeEnd(id) => {
                    if self.downtimes.get(&id).is_some_and(|d| d.active) {
                        self.stop_downtime(id, false, now, out);
                    }
                }
                Timer::DowntimeExpire(id) => {
                    if self.downtimes.get(&id).is_some_and(|d| !d.active) {
                        self.stop_downtime(id, false, now, out);
                    }
                }
                Timer::PerfdataProcessing(place) => self.process_perfdata(place, at, now, out),
                Timer::Freshness(kind) => self.check_freshness(kind == 0, at, now, out),
            }
        }
    }

    pub(crate) fn next_timer(&self) -> Option<Ms> {
        self.timers.iter().next().map(|&(at, _, _)| at)
    }

    fn new_id(&mut self) -> String {
        self.uuid_pub()
    }
}

/// The parts of an escalation that notifications look at, whether it is
/// one of a host or of a service.
struct EscalationView<'a> {
    first_notification: Option<u32>,
    last_notification: Option<u32>,
    notification_interval: f64,
    period: &'a Option<String>,
    options: &'a Opts,
    contacts: &'a [String],
    contact_groups: &'a [String],
}

impl<'a> EscalationView<'a> {
    fn of<Id>(e: &'a checkd_config::model::Escalation<Id>) -> EscalationView<'a> {
        EscalationView {
            first_notification: e.first_notification,
            last_notification: e.last_notification,
            notification_interval: e.notification_interval,
            period: &e.period,
            options: &e.options,
            contacts: &e.contacts,
            contact_groups: &e.contact_groups,
        }
    }
}

/// Builds the contacts and the escalations of each object.
pub(crate) fn contacts_of(objects: &checkd_config::model::Objects) -> (Vec<ContactEntry>, BTreeMap<String, usize>) {
    let mut list = Vec::new();
    let mut index = BTreeMap::new();
    for contact in objects.contacts.values() {
        index.insert(contact.name.clone(), list.len());
        list.push(ContactEntry {
            cfg: contact.clone(),
            reason: [None, None],
        });
    }
    (list, index)
}

/// The macros of a notification, by name, where `notice` is the
/// notification at hand.
pub(crate) fn notice_value(engine: &Engine, notice: &Notice, obj: Obj, name: &str) -> Option<String> {
    let item = engine.item(obj);
    let host_item = match obj {
        Obj::Host(at) => &engine.hosts[at].item,
        Obj::Service(at) => &engine.hosts[engine.services[at].host].item,
    };
    Some(match name {
        "NOTIFICATIONTYPE" => notice.kind.clone(),
        "NOTIFICATIONNUMBER" => item.notice.number.to_string(),
        "NOTIFICATIONRECIPIENTS" => notice.recipients.clone(),
        "NOTIFICATIONISESCALATED" => (notice.escalated as u8).to_string(),
        "NOTIFICATIONAUTHOR" | "NOTIFICATIONAUTHORNAME" | "NOTIFICATIONAUTHORALIAS" => notice.author.clone(),
        "NOTIFICATIONCOMMENT" => notice.comment.clone(),
        "HOSTNOTIFICATIONNUMBER" => host_item.notice.number.to_string(),
        "HOSTNOTIFICATIONID" => match obj {
            Obj::Host(_) => notice.id.clone(),
            Obj::Service(_) => host_item.notice.id.clone(),
        },
        "SERVICENOTIFICATIONNUMBER" if matches!(obj, Obj::Service(_)) => item.notice.number.to_string(),
        "SERVICENOTIFICATIONID" if matches!(obj, Obj::Service(_)) => notice.id.clone(),
        "CONTACTNAME" => notice.contact.clone(),
        "CONTACTALIAS" | "CONTACTEMAIL" | "CONTACTPAGER" => {
            let at = *engine.contact_index.get(&notice.contact)?;
            let contact = &engine.contacts[at].cfg;
            match name {
                "CONTACTALIAS" => contact.alias.clone(),
                "CONTACTEMAIL" => contact.email.clone().unwrap_or_default(),
                _ => contact.pager.clone().unwrap_or_default(),
            }
        }
        _ => {
            if let Some(n) = name.strip_prefix("CONTACTADDRESS").and_then(|n| n.parse::<usize>().ok()) {
                let at = *engine.contact_index.get(&notice.contact)?;
                if (1..=6).contains(&n) {
                    return Some(engine.contacts[at].cfg.addresses[n - 1].clone().unwrap_or_default());
                }
            }
            if let Some(var) = name.strip_prefix("_CONTACT") {
                let at = *engine.contact_index.get(&notice.contact)?;
                let custom = &engine.contacts[at].cfg.custom;
                return custom
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(var))
                    .map(|(_, value)| value.clone());
            }
            return None;
        }
    })
}

/// The state names, for the log.
pub fn state_name(obj: Obj, state: u8) -> &'static str {
    match obj {
        Obj::Host(_) => HOST_STATES[state as usize],
        Obj::Service(_) => SERVICE_STATES[state as usize],
    }
}
