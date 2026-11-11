//! Macros: the `$NAME$` places in a command line and what goes into them.
//!
//! - The text between two `$` is a macro name. A name that is not known,
//!   or that has nothing to stand for where it is used (a service macro in
//!   a host check), is left as it was written, with both `$`.
//! - `$$` gives one `$`. A `$` on its own pairs with the next one, however
//!   far away.
//! - `$ARG1$` and up are the parts of `check_command` after each `!`. A
//!   `!` with a backslash before it is not a divider. Macros in the parts
//!   are filled in before the parts are put into the command's line.
//! - `$USER1$` and up come from the resource file. One that is not set is
//!   empty. What a macro stands for is not looked through for macros
//!   again, except for the notes and the two URLs of an object.
//! - `$_HOSTNAME$` and `$_SERVICENAME$` are custom variables.
//! - A macro of another object is asked for with its name after a colon:
//!   `$HOSTSTATE:b$`, `$SERVICESTATE:b:t$`, `$SERVICESTATE::t$` for
//!   service `t` of the host at hand, and `$HOSTSTATEID:group:,$` for all
//!   hosts of a host group with `,` between them.
//!
//! Members of a group are given in the order of their names. The
//! reference has its own order for service and contact groups.

use checkd_config::model::Group;

use crate::engine::{Engine, Ms, Obj, HOST_STATES, SERVICE_STATES};
use crate::notify::{self, Notice};
use crate::settings::DateFormat;
use crate::timeperiod::civil;

/// How often notes and URLs may bring in notes and URLs.
const DEPTH: usize = 4;

#[derive(Clone, Copy)]
struct Ctx<'a> {
    host: Option<usize>,
    service: Option<usize>,
    args: &'a [String],
    /// The notification a command is run for.
    notice: Option<&'a Notice>,
}

impl Engine {
    /// The command line for a `check_command` or `event_handler` value:
    /// the named command's line with all macros filled in. `None` when
    /// there is no command of that name.
    pub(crate) fn command_line(&self, command: &str, obj: Obj, now: Ms) -> Option<String> {
        let mut parts = split_args(command);
        let name = parts.remove(0);
        let line = &self.objects.commands.get(name.trim())?.line;
        let ctx = self.ctx(obj, &[]);
        let args: Vec<String> = parts
            .iter()
            .map(|part| self.expand_in(part, ctx, now, 0))
            .collect();
        Some(self.expand_in(line, self.ctx(obj, &args), now, 0))
    }

    /// The command line of a notification command for one contact.
    pub(crate) fn notice_line(&self, command: &str, obj: Obj, notice: &Notice, now: Ms) -> Option<String> {
        let mut parts = split_args(command);
        let name = parts.remove(0);
        let line = &self.objects.commands.get(name.trim())?.line;
        let mut ctx = self.ctx(obj, &[]);
        ctx.notice = Some(notice);
        let args: Vec<String> = parts.iter().map(|part| self.expand_in(part, ctx, now, 0)).collect();
        let mut ctx = self.ctx(obj, &args);
        ctx.notice = Some(notice);
        Some(self.expand_in(line, ctx, now, 0))
    }

    /// Fills in the macros of a text, as seen from a host or service.
    pub fn expand(&self, text: &str, obj: Obj, now: Ms) -> String {
        self.expand_in(text, self.ctx(obj, &[]), now, 0)
    }

    fn ctx<'a>(&self, obj: Obj, args: &'a [String]) -> Ctx<'a> {
        match obj {
            Obj::Host(at) => Ctx {
                host: Some(at),
                service: None,
                args,
                notice: None,
            },
            Obj::Service(at) => Ctx {
                host: Some(self.services[at].host),
                service: Some(at),
                args,
                notice: None,
            },
        }
    }

    fn expand_in(&self, text: &str, ctx: Ctx, now: Ms, depth: usize) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(open) = rest.find('$') {
            out.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            let Some(close) = after.find('$') else {
                out.push_str(&rest[open..]);
                return out;
            };
            let name = &after[..close];
            if name.is_empty() {
                out.push('$');
            } else if let Some(value) = self.lookup(name, ctx, now, depth) {
                out.push_str(&value);
            } else {
                out.push('$');
                out.push_str(name);
                out.push('$');
            }
            rest = &after[close + 1..];
        }
        out.push_str(rest);
        out
    }

    fn lookup(&self, name: &str, ctx: Ctx, now: Ms, depth: usize) -> Option<String> {
        if let Some(n) = number_after(name, "ARG") {
            if (1..=32).contains(&n) {
                return Some(ctx.args.get(n - 1).cloned().unwrap_or_default());
            }
        }
        if let Some(n) = number_after(name, "USER") {
            if (1..=256).contains(&n) {
                return Some(self.user_macros.get(&(n as u32)).cloned().unwrap_or_default());
            }
        }
        let mut parts = name.split(':');
        let base = parts.next().unwrap_or("");
        let args: Vec<&str> = parts.collect();

        if let (Some(notice), true) = (ctx.notice, args.is_empty()) {
            let obj = match ctx.service {
                Some(at) => Obj::Service(at),
                None => Obj::Host(ctx.host?),
            };
            if let Some(value) = notify::notice_value(self, notice, obj, base) {
                return Some(value);
            }
        }

        if let Some(var) = base.strip_prefix("_HOST") {
            let host = match args.as_slice() {
                [] => ctx.host?,
                [name] => *self.host_index.get(*name)?,
                _ => return None,
            };
            let custom = &self.hosts[host].item.cfg.custom;
            return find_custom(custom, var);
        }
        if let Some(var) = base.strip_prefix("_SERVICE") {
            let service = match args.as_slice() {
                [] => ctx.service?,
                [host, description] => self.service_named(host, description, ctx)?,
                _ => return None,
            };
            return find_custom(&self.services[service].item.cfg.custom, var);
        }
        if base.starts_with('_') {
            return None;
        }

        let second = now.div_euclid(1000);
        if base == "ISVALIDTIME" || base == "NEXTVALIDTIME" {
            let (period, moment) = match args.as_slice() {
                [period] => (*period, second),
                [period, moment] => (*period, moment.trim().parse().ok()?),
                _ => return None,
            };
            if !self.objects.timeperiods.contains_key(period) {
                return None;
            }
            let periods = crate::timeperiod::Periods::new(&self.objects.timeperiods, &*self.zone);
            return Some(if base == "ISVALIDTIME" {
                (periods.is_valid(period, moment) as u8).to_string()
            } else {
                // A moment that is past counts as now.
                periods
                    .next_valid(period, moment.max(second))
                    .unwrap_or(0)
                    .to_string()
            });
        }

        // Macros of a host.
        match args.as_slice() {
            [] => {
                if let Some(value) = ctx.host.and_then(|at| self.host_value(at, base, now, depth)) {
                    return Some(value);
                }
            }
            [name] => {
                if let Some(&at) = self.host_index.get(*name) {
                    if let Some(value) = self.host_value(at, base, now, depth) {
                        return Some(value);
                    }
                }
            }
            [group, between] => {
                if let Some(group) = self.objects.hostgroups.get(*group) {
                    let values: Option<Vec<String>> = group
                        .members
                        .iter()
                        .filter_map(|name| self.host_index.get(name))
                        .map(|&at| self.host_value(at, base, now, depth))
                        .collect();
                    if let Some(values) = values {
                        return Some(values.join(between));
                    }
                }
            }
            _ => {}
        }

        // Macros of a service.
        match args.as_slice() {
            [] => {
                if let Some(value) = ctx
                    .service
                    .and_then(|at| self.service_value(at, base, now, depth))
                {
                    return Some(value);
                }
            }
            [first, second] => {
                if let Some(at) = self.service_named(first, second, ctx) {
                    if let Some(value) = self.service_value(at, base, now, depth) {
                        return Some(value);
                    }
                } else if let Some(group) = self.objects.servicegroups.get(*first) {
                    let values: Option<Vec<String>> = group
                        .members
                        .iter()
                        .filter_map(|id| self.service_index.get(id))
                        .map(|&at| self.service_value(at, base, now, depth))
                        .collect();
                    if let Some(values) = values {
                        return Some(values.join(second));
                    }
                }
            }
            _ => {}
        }

        // Macros of groups: of the first group the object is in, or of
        // the group that is named.
        if let Some(what) = base.strip_prefix("HOSTGROUP") {
            let group = match args.as_slice() {
                [] => ctx.host.and_then(|at| self.hosts[at].groups.first()).cloned(),
                [name] => Some(name.to_string()),
                _ => None,
            };
            if let Some(group) = group.and_then(|name| self.objects.hostgroups.get(&name)) {
                if what == "MEMBERS" {
                    let members: Vec<&str> = group.members.iter().map(String::as_str).collect();
                    return Some(members.join(","));
                }
                return group_value(group, what);
            }
            return if args.is_empty() && is_group_part(what) { Some(String::new()) } else { None };
        }
        if let Some(what) = base.strip_prefix("SERVICEGROUP") {
            // Where there is no service, as in a host check, the
            // reference leaves these as they are written.
            if args.is_empty() && ctx.service.is_none() {
                return None;
            }
            let group = match args.as_slice() {
                [] => ctx.service.and_then(|at| self.services[at].groups.first()).cloned(),
                [name] => Some(name.to_string()),
                _ => None,
            };
            if let Some(group) = group.and_then(|name| self.objects.servicegroups.get(&name)) {
                if what == "MEMBERS" {
                    let members: Vec<String> = group
                        .members
                        .iter()
                        .map(|(host, description)| format!("{host},{description}"))
                        .collect();
                    return Some(members.join(","));
                }
                return group_value(group, what);
            }
            return if args.is_empty() && is_group_part(what) { Some(String::new()) } else { None };
        }

        // Contacts are known here only by name. The contact a message
        // goes to comes with notifications.
        if let [name] = args.as_slice() {
            if let Some(what) = base.strip_prefix("CONTACTGROUP") {
                let group = self.objects.contactgroups.get(*name)?;
                if what == "MEMBERS" {
                    let members: Vec<&str> = group.members.iter().map(String::as_str).collect();
                    return Some(members.join(","));
                }
                return group_value(group, what);
            }
            if let Some(what) = base.strip_prefix("CONTACT") {
                let contact = self.objects.contacts.get(*name)?;
                let address = |n: usize| contact.addresses.get(n)?.clone();
                return match what {
                    "NAME" => Some(contact.name.clone()),
                    "ALIAS" => Some(contact.alias.clone()),
                    "EMAIL" => Some(contact.email.clone().unwrap_or_default()),
                    "PAGER" => Some(contact.pager.clone().unwrap_or_default()),
                    _ => {
                        let n = number_after(what, "ADDRESS").filter(|n| (1..=6).contains(n))?;
                        Some(address(n - 1).unwrap_or_default())
                    }
                };
            }
        }

        if !args.is_empty() {
            return None;
        }
        self.global_value(base, now)
    }

    /// The service a macro names with `host:description`. An empty host
    /// is the host at hand.
    fn service_named(&self, host: &str, description: &str, ctx: Ctx) -> Option<usize> {
        let host = if host.is_empty() {
            self.hosts[ctx.host?].name.clone()
        } else {
            host.to_string()
        };
        self.service_index
            .get(&(host, description.to_string()))
            .copied()
    }

    fn host_value(&self, at: usize, name: &str, now: Ms, depth: usize) -> Option<String> {
        let entry = &self.hosts[at];
        let item = &entry.item;
        let host = self.objects.hosts.get(&entry.name)?;
        let again = |text: &Option<String>| {
            let text = text.clone().unwrap_or_default();
            if depth >= DEPTH {
                return text;
            }
            let ctx = Ctx {
                host: Some(at),
                service: None,
                args: &[],
                notice: None,
            };
            self.expand_in(&text, ctx, now, depth + 1)
        };
        let count = |state: Option<u8>| {
            entry
                .services
                .iter()
                .filter(|&&s| state.is_none_or(|state| self.services[s].item.state == state))
                .count()
                .to_string()
        };
        Some(match name {
            "HOSTNAME" => entry.name.clone(),
            "HOSTDISPLAYNAME" => item.cfg.display_name.clone().unwrap_or(entry.name.clone()),
            "HOSTALIAS" => host.alias.clone(),
            "HOSTADDRESS" => host.address.clone(),
            "HOSTSTATE" => HOST_STATES[item.state as usize].to_string(),
            "HOSTSTATEID" => item.state.to_string(),
            "LASTHOSTSTATE" => HOST_STATES[item.last_state as usize].to_string(),
            "LASTHOSTSTATEID" => item.last_state.to_string(),
            "HOSTSTATETYPE" => item.state_type().to_string(),
            "HOSTATTEMPT" => item.attempt.to_string(),
            "MAXHOSTATTEMPTS" => item.cfg.max_check_attempts.to_string(),
            "HOSTEVENTID" => item.event_id.to_string(),
            "LASTHOSTEVENTID" => item.last_event_id.to_string(),
            "HOSTPROBLEMID" => item.problem_id.clone(),
            "LASTHOSTPROBLEMID" => item.last_problem_id.clone(),
            "HOSTLATENCY" => format!("{:.3}", item.latency),
            "HOSTEXECUTIONTIME" => format!("{:.3}", item.execution_time),
            "HOSTDURATION" => duration(self.since(item.last_state_change, now)),
            "HOSTDURATIONSEC" => self.since(item.last_state_change, now).to_string(),
            "HOSTDOWNTIME" => item.notice.downtime_depth.to_string(),
            "HOSTACKAUTHOR" | "HOSTACKAUTHORNAME" | "HOSTACKAUTHORALIAS" => {
                item.notice.ack.as_ref().map(|ack| ack.author.clone()).unwrap_or_default()
            }
            "HOSTACKCOMMENT" => item.notice.ack.as_ref().map(|ack| ack.comment.clone()).unwrap_or_default(),
            "HOSTPERCENTCHANGE" => format!("{:.2}", item.percent_change),
            "HOSTGROUPNAME" => entry.groups.first().cloned().unwrap_or_default(),
            "HOSTGROUPNAMES" => entry.groups.join(","),
            "LASTHOSTCHECK" => seconds(item.last_check),
            "LASTHOSTSTATECHANGE" => seconds(item.last_state_change),
            "LASTHOSTUP" => seconds(item.last_time[0]),
            "LASTHOSTDOWN" => seconds(item.last_time[1]),
            "LASTHOSTUNREACHABLE" => seconds(item.last_time[2]),
            "HOSTOUTPUT" => self.clean(&item.output.short),
            "LONGHOSTOUTPUT" => self.clean(&item.output.long),
            "HOSTPERFDATA" => self.clean(&item.output.perfdata),
            "HOSTCHECKCOMMAND" => item.cfg.check_command.clone().unwrap_or_default(),
            "HOSTCHECKTYPE" => "ACTIVE".to_string(),
            "HOSTACTIONURL" => again(&item.cfg.action_url),
            "HOSTNOTESURL" => again(&item.cfg.notes_url),
            "HOSTNOTES" => again(&item.cfg.notes),
            "HOSTNOTIFICATIONNUMBER" => item.notice.number.to_string(),
            "HOSTNOTIFICATIONID" => item.notice.id.clone(),
            "TOTALHOSTSERVICES" => count(None),
            // A service that has not been checked yet counts as OK here.
            "TOTALHOSTSERVICESOK" => count(Some(0)),
            "TOTALHOSTSERVICESWARNING" => count(Some(1)),
            "TOTALHOSTSERVICESCRITICAL" => count(Some(2)),
            "TOTALHOSTSERVICESUNKNOWN" => count(Some(3)),
            _ => return None,
        })
    }

    fn service_value(&self, at: usize, name: &str, now: Ms, depth: usize) -> Option<String> {
        let entry = &self.services[at];
        let item = &entry.item;
        let again = |text: &Option<String>| {
            let text = text.clone().unwrap_or_default();
            if depth >= DEPTH {
                return text;
            }
            let ctx = Ctx {
                host: Some(entry.host),
                service: Some(at),
                args: &[],
                notice: None,
            };
            self.expand_in(&text, ctx, now, depth + 1)
        };
        Some(match name {
            "SERVICEDESC" => entry.description.clone(),
            "SERVICEDISPLAYNAME" => item
                .cfg
                .display_name
                .clone()
                .unwrap_or(entry.description.clone()),
            "SERVICESTATE" => SERVICE_STATES[item.state as usize].to_string(),
            "SERVICESTATEID" => item.state.to_string(),
            "LASTSERVICESTATE" => SERVICE_STATES[item.last_state as usize].to_string(),
            "LASTSERVICESTATEID" => item.last_state.to_string(),
            "SERVICESTATETYPE" => item.state_type().to_string(),
            "SERVICEATTEMPT" => item.attempt.to_string(),
            "MAXSERVICEATTEMPTS" => item.cfg.max_check_attempts.to_string(),
            "SERVICEISVOLATILE" => (entry.volatile as u8).to_string(),
            "SERVICEEVENTID" => item.event_id.to_string(),
            "LASTSERVICEEVENTID" => item.last_event_id.to_string(),
            "SERVICEPROBLEMID" => item.problem_id.clone(),
            "LASTSERVICEPROBLEMID" => item.last_problem_id.clone(),
            "SERVICELATENCY" => format!("{:.3}", item.latency),
            "SERVICEEXECUTIONTIME" => format!("{:.3}", item.execution_time),
            "SERVICEDURATION" => duration(self.since(item.last_state_change, now)),
            "SERVICEDURATIONSEC" => self.since(item.last_state_change, now).to_string(),
            "SERVICEDOWNTIME" => item.notice.downtime_depth.to_string(),
            "SERVICEACKAUTHOR" | "SERVICEACKAUTHORNAME" | "SERVICEACKAUTHORALIAS" => {
                item.notice.ack.as_ref().map(|ack| ack.author.clone()).unwrap_or_default()
            }
            "SERVICEACKCOMMENT" => item.notice.ack.as_ref().map(|ack| ack.comment.clone()).unwrap_or_default(),
            "SERVICEPERCENTCHANGE" => format!("{:.2}", item.percent_change),
            "SERVICEGROUPNAME" => entry.groups.first().cloned().unwrap_or_default(),
            "SERVICEGROUPNAMES" => entry.groups.join(","),
            "LASTSERVICECHECK" => seconds(item.last_check),
            "LASTSERVICESTATECHANGE" => seconds(item.last_state_change),
            "LASTSERVICEOK" => seconds(item.last_time[0]),
            "LASTSERVICEWARNING" => seconds(item.last_time[1]),
            "LASTSERVICECRITICAL" => seconds(item.last_time[2]),
            "LASTSERVICEUNKNOWN" => seconds(item.last_time[3]),
            "SERVICEOUTPUT" => self.clean(&item.output.short),
            "LONGSERVICEOUTPUT" => self.clean(&item.output.long),
            "SERVICEPERFDATA" => self.clean(&item.output.perfdata),
            "SERVICECHECKCOMMAND" => item.cfg.check_command.clone().unwrap_or_default(),
            "SERVICECHECKTYPE" => "ACTIVE".to_string(),
            "SERVICEACTIONURL" => again(&item.cfg.action_url),
            "SERVICENOTESURL" => again(&item.cfg.notes_url),
            "SERVICENOTES" => again(&item.cfg.notes),
            "SERVICENOTIFICATIONNUMBER" => item.notice.number.to_string(),
            "SERVICENOTIFICATIONID" => item.notice.id.clone(),
            _ => return None,
        })
    }

    fn global_value(&self, name: &str, now: Ms) -> Option<String> {
        // Only objects that have been checked are counted.
        let hosts = |state: Option<u8>| {
            self.hosts
                .iter()
                .filter(|h| h.item.checked || h.item.state != 0)
                .filter(|h| match state {
                    Some(state) => h.item.state == state,
                    None => h.item.state != 0,
                })
                .count()
                .to_string()
        };
        let services = |state: Option<u8>| {
            self.services
                .iter()
                .filter(|s| s.item.checked || s.item.state != 0)
                .filter(|s| match state {
                    Some(state) => s.item.state == state,
                    None => s.item.state != 0,
                })
                .count()
                .to_string()
        };
        let second = now.div_euclid(1000);
        Some(match name {
            "TOTALHOSTSUP" => hosts(Some(0)),
            "TOTALHOSTSDOWN" => hosts(Some(1)),
            "TOTALHOSTSUNREACHABLE" => hosts(Some(2)),
            "TOTALHOSTPROBLEMS" => hosts(None),
            "TOTALSERVICESOK" => services(Some(0)),
            "TOTALSERVICESWARNING" => services(Some(1)),
            "TOTALSERVICESCRITICAL" => services(Some(2)),
            "TOTALSERVICESUNKNOWN" => services(Some(3)),
            "TOTALSERVICEPROBLEMS" => services(None),
            "ADMINEMAIL" => self.settings.admin_email.clone(),
            "ADMINPAGER" => self.settings.admin_pager.clone(),
            "TIMET" => second.to_string(),
            "PROCESSSTARTTIME" | "EVENTSTARTTIME" => self.start.div_euclid(1000).to_string(),
            "TIME" => self.clock(second).time,
            "DATE" => self.clock(second).date,
            "SHORTDATETIME" => {
                let clock = self.clock(second);
                let between = if self.settings.date_format == DateFormat::StrictIso8601 {
                    "T"
                } else {
                    " "
                };
                format!("{}{between}{}", clock.date, clock.time)
            }
            "LONGDATETIME" => self.clock(second).long,
            // What notifications fill in. Empty outside one.
            "NOTIFICATIONTYPE" => String::new(),
            _ => return self.paths.get(name).cloned(),
        })
    }

    /// A moment as `$SHORTDATETIME$` writes it.
    pub(crate) fn short_datetime(&self, second: i64) -> String {
        let clock = self.clock(second);
        let between = if self.settings.date_format == DateFormat::StrictIso8601 { "T" } else { " " };
        format!("{}{between}{}", clock.date, clock.time)
    }

    /// Whole seconds since a moment. For an object that has not changed
    /// state yet the moment is the start of the engine.
    fn since(&self, moment: Ms, now: Ms) -> i64 {
        let from = if moment > 0 { moment } else { self.start };
        (now.div_euclid(1000) - from.div_euclid(1000)).max(0)
    }

    /// Takes out the characters of `illegal_macro_output_chars`.
    fn clean(&self, text: &str) -> String {
        let illegal = &self.settings.illegal_macro_output_chars;
        if illegal.is_empty() {
            return text.to_string();
        }
        text.chars().filter(|c| !illegal.contains(*c)).collect()
    }

    fn clock(&self, second: i64) -> Clock {
        let local = second + self.zone.offset(second);
        let day = local.div_euclid(86400);
        let in_day = local.rem_euclid(86400);
        let (year, month, mday) = civil(day);
        let (h, m, s) = (in_day / 3600, in_day / 60 % 60, in_day % 60);
        let date = match self.settings.date_format {
            DateFormat::Us => format!("{month:02}-{mday:02}-{year:04}"),
            DateFormat::Euro => format!("{mday:02}-{month:02}-{year:04}"),
            DateFormat::Iso8601 | DateFormat::StrictIso8601 => {
                format!("{year:04}-{month:02}-{mday:02}")
            }
        };
        const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let weekday = DAYS[(day + 4).rem_euclid(7) as usize];
        let long = format!(
            "{weekday} {} {mday:2} {h:02}:{m:02}:{s:02} {} {year}",
            MONTHS[(month - 1) as usize],
            self.zone.name(second)
        );
        Clock {
            time: format!("{h:02}:{m:02}:{s:02}"),
            date,
            long,
        }
    }
}

struct Clock {
    time: String,
    date: String,
    long: String,
}

fn is_group_part(what: &str) -> bool {
    matches!(what, "ALIAS" | "MEMBERS" | "NOTES" | "NOTESURL" | "ACTIONURL")
}

fn group_value<M: Ord>(group: &Group<M>, what: &str) -> Option<String> {
    Some(match what {
        "ALIAS" => group.alias.clone(),
        "NOTES" => group.notes.clone().unwrap_or_default(),
        "NOTESURL" => group.notes_url.clone().unwrap_or_default(),
        "ACTIONURL" => group.action_url.clone().unwrap_or_default(),
        _ => return None,
    })
}

fn find_custom(custom: &[(String, String)], var: &str) -> Option<String> {
    custom
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(var))
        .map(|(_, value)| value.clone())
}

/// `ARG3` with prefix `ARG` is 3.
fn number_after(name: &str, prefix: &str) -> Option<usize> {
    let digits = name.strip_prefix(prefix)?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// A time in whole seconds, 0 for never.
fn seconds(moment: Ms) -> String {
    moment.div_euclid(1000).to_string()
}

fn duration(seconds: i64) -> String {
    format!(
        "{}d {}h {}m {}s",
        seconds / 86400,
        seconds / 3600 % 24,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// Splits `name!arg1!arg2` at each `!` that has no backslash before it.
/// The first piece is the command's name.
pub fn split_args(command: &str) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'!') {
            chars.next();
            parts.last_mut().unwrap().push('!');
        } else if c == '!' {
            parts.push(String::new());
        } else {
            parts.last_mut().unwrap().push(c);
        }
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments() {
        assert_eq!(
            split_args(r"mac3!sm3!two words!with\!bang!$HOSTNAME$"),
            ["mac3", "sm3", "two words", "with!bang", "$HOSTNAME$"]
        );
        assert_eq!(split_args("plain"), ["plain"]);
        assert_eq!(split_args("c!!x"), ["c", "", "x"]);
    }

    #[test]
    fn durations() {
        assert_eq!(duration(2), "0d 0h 0m 2s");
        assert_eq!(duration(90061), "1d 1h 1m 1s");
    }
}
