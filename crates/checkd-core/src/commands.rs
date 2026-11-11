//! External commands: the lines that come through the command file.
//!
//! A line is `[<seconds since 1970>] NAME;argument;argument...`. The
//! service of a service command counts as one argument, written as two
//! fields: `host;description`. The last argument of a command that ends
//! in a text takes the rest of the line, `;` and all.
//!
//! What the log shows, as the reference writes it:
//!
//! - a command that is understood: `EXTERNAL COMMAND: NAME;arguments`,
//!   before it is carried out;
//! - one that is not: `Warning: External command parse error [<time>]
//!   NAME;arguments (<why>)` and `External command error: <why>`. The
//!   reasons are `Unknown command 'NAME'`, `Missing argument: expected N
//!   arguments, found M` and `Failed validation of <what> as type <type>
//!   (argument <n>)`;
//! - one that is understood but cannot be carried out, such as a downtime
//!   that ends before it starts or one that does not exist:
//!   `Error: External command failed -> NAME;arguments`. Some commands
//!   fail without a word: an acknowledgement of a host or service that
//!   has no hard problem.

use crate::engine::{
    Effect, Engine, Ms, Obj, MOD_ACTIVE_CHECKS, MOD_CUSTOM_VARIABLE, MOD_EVENT_HANDLER, MOD_FLAP_DETECTION,
    MOD_MAX_ATTEMPTS, MOD_NORMAL_INTERVAL, MOD_NOTIFICATIONS, MOD_PASSIVE_CHECKS, MOD_RETRY_INTERVAL,
};
use crate::notify::Kind;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Arg {
    Host,
    Service,
    HostGroup,
    ServiceGroup,
    Int,
    Bool,
    /// A count of interval units, with the name the reference gives it in
    /// its error.
    Interval(&'static str),
    /// A text that takes the rest of the line.
    Text,
    /// A text that is one field.
    Word,
}

use Arg::*;

/// The commands checkd knows, with their arguments.
const COMMANDS: &[(&str, &[Arg])] = &[
    ("ACKNOWLEDGE_HOST_PROBLEM", &[Host, Int, Bool, Bool, Text, Text]),
    ("ACKNOWLEDGE_SVC_PROBLEM", &[Service, Int, Bool, Bool, Text, Text]),
    ("REMOVE_HOST_ACKNOWLEDGEMENT", &[Host]),
    ("REMOVE_SVC_ACKNOWLEDGEMENT", &[Service]),
    ("SCHEDULE_HOST_DOWNTIME", &[Host, Int, Int, Bool, Int, Int, Text, Text]),
    ("SCHEDULE_SVC_DOWNTIME", &[Service, Int, Int, Bool, Int, Int, Text, Text]),
    ("DEL_HOST_DOWNTIME", &[Int]),
    ("DEL_SVC_DOWNTIME", &[Int]),
    ("ADD_HOST_COMMENT", &[Host, Bool, Text, Text]),
    ("ADD_SVC_COMMENT", &[Service, Bool, Text, Text]),
    ("DEL_HOST_COMMENT", &[Int]),
    ("DEL_SVC_COMMENT", &[Int]),
    ("ENABLE_HOST_NOTIFICATIONS", &[Host]),
    ("DISABLE_HOST_NOTIFICATIONS", &[Host]),
    ("ENABLE_SVC_NOTIFICATIONS", &[Service]),
    ("DISABLE_SVC_NOTIFICATIONS", &[Service]),
    ("ENABLE_NOTIFICATIONS", &[]),
    ("DISABLE_NOTIFICATIONS", &[]),
    ("PROCESS_SERVICE_CHECK_RESULT", &[Service, Int, Text]),
    ("PROCESS_HOST_CHECK_RESULT", &[Host, Int, Text]),
    ("SCHEDULE_SVC_CHECK", &[Service, Int]),
    ("SCHEDULE_FORCED_SVC_CHECK", &[Service, Int]),
    ("SCHEDULE_HOST_CHECK", &[Host, Int]),
    ("SCHEDULE_FORCED_HOST_CHECK", &[Host, Int]),
    ("SCHEDULE_HOST_SVC_CHECKS", &[Host, Int]),
    ("SCHEDULE_FORCED_HOST_SVC_CHECKS", &[Host, Int]),
    ("ENABLE_SVC_CHECK", &[Service]),
    ("DISABLE_SVC_CHECK", &[Service]),
    ("ENABLE_HOST_CHECK", &[Host]),
    ("DISABLE_HOST_CHECK", &[Host]),
    ("ENABLE_SVC_EVENT_HANDLER", &[Service]),
    ("DISABLE_SVC_EVENT_HANDLER", &[Service]),
    ("ENABLE_HOST_EVENT_HANDLER", &[Host]),
    ("DISABLE_HOST_EVENT_HANDLER", &[Host]),
    ("ENABLE_SVC_FLAP_DETECTION", &[Service]),
    ("DISABLE_SVC_FLAP_DETECTION", &[Service]),
    ("ENABLE_HOST_FLAP_DETECTION", &[Host]),
    ("DISABLE_HOST_FLAP_DETECTION", &[Host]),
    ("ENABLE_PASSIVE_SVC_CHECKS", &[Service]),
    ("DISABLE_PASSIVE_SVC_CHECKS", &[Service]),
    ("ENABLE_PASSIVE_HOST_CHECKS", &[Host]),
    ("DISABLE_PASSIVE_HOST_CHECKS", &[Host]),
    ("ENABLE_HOST_SVC_CHECKS", &[Host]),
    ("DISABLE_HOST_SVC_CHECKS", &[Host]),
    ("ENABLE_HOST_SVC_NOTIFICATIONS", &[Host]),
    ("DISABLE_HOST_SVC_NOTIFICATIONS", &[Host]),
    ("ENABLE_HOSTGROUP_SVC_NOTIFICATIONS", &[HostGroup]),
    ("DISABLE_HOSTGROUP_SVC_NOTIFICATIONS", &[HostGroup]),
    ("ENABLE_HOSTGROUP_HOST_NOTIFICATIONS", &[HostGroup]),
    ("DISABLE_HOSTGROUP_HOST_NOTIFICATIONS", &[HostGroup]),
    ("ENABLE_HOSTGROUP_SVC_CHECKS", &[HostGroup]),
    ("DISABLE_HOSTGROUP_SVC_CHECKS", &[HostGroup]),
    ("ENABLE_HOSTGROUP_HOST_CHECKS", &[HostGroup]),
    ("DISABLE_HOSTGROUP_HOST_CHECKS", &[HostGroup]),
    ("ENABLE_SERVICEGROUP_SVC_NOTIFICATIONS", &[ServiceGroup]),
    ("DISABLE_SERVICEGROUP_SVC_NOTIFICATIONS", &[ServiceGroup]),
    ("ENABLE_SERVICEGROUP_HOST_NOTIFICATIONS", &[ServiceGroup]),
    ("DISABLE_SERVICEGROUP_HOST_NOTIFICATIONS", &[ServiceGroup]),
    ("ENABLE_SERVICEGROUP_SVC_CHECKS", &[ServiceGroup]),
    ("DISABLE_SERVICEGROUP_SVC_CHECKS", &[ServiceGroup]),
    ("ENABLE_SERVICEGROUP_HOST_CHECKS", &[ServiceGroup]),
    ("DISABLE_SERVICEGROUP_HOST_CHECKS", &[ServiceGroup]),
    ("ENABLE_EVENT_HANDLERS", &[]),
    ("DISABLE_EVENT_HANDLERS", &[]),
    ("ENABLE_FLAP_DETECTION", &[]),
    ("DISABLE_FLAP_DETECTION", &[]),
    ("START_ACCEPTING_PASSIVE_SVC_CHECKS", &[]),
    ("STOP_ACCEPTING_PASSIVE_SVC_CHECKS", &[]),
    ("START_ACCEPTING_PASSIVE_HOST_CHECKS", &[]),
    ("STOP_ACCEPTING_PASSIVE_HOST_CHECKS", &[]),
    ("CHANGE_NORMAL_SVC_CHECK_INTERVAL", &[Service, Interval("check_interval")]),
    ("CHANGE_RETRY_SVC_CHECK_INTERVAL", &[Service, Interval("retry_interval")]),
    ("CHANGE_NORMAL_HOST_CHECK_INTERVAL", &[Host, Interval("check_interval")]),
    ("CHANGE_RETRY_HOST_CHECK_INTERVAL", &[Host, Interval("retry_interval")]),
    ("CHANGE_MAX_SVC_CHECK_ATTEMPTS", &[Service, Int]),
    ("CHANGE_MAX_HOST_CHECK_ATTEMPTS", &[Host, Int]),
    ("CHANGE_CUSTOM_SVC_VAR", &[Service, Word, Text]),
    ("CHANGE_CUSTOM_HOST_VAR", &[Host, Word, Text]),
    ("DEL_ALL_HOST_COMMENTS", &[Host]),
    ("DEL_ALL_SVC_COMMENTS", &[Service]),
    ("SCHEDULE_HOSTGROUP_HOST_DOWNTIME", &[HostGroup, Int, Int, Bool, Int, Int, Text, Text]),
    ("SCHEDULE_HOSTGROUP_SVC_DOWNTIME", &[HostGroup, Int, Int, Bool, Int, Int, Text, Text]),
    ("SCHEDULE_SERVICEGROUP_SVC_DOWNTIME", &[ServiceGroup, Int, Int, Bool, Int, Int, Text, Text]),
    ("SCHEDULE_SERVICEGROUP_HOST_DOWNTIME", &[ServiceGroup, Int, Int, Bool, Int, Int, Text, Text]),
    ("SCHEDULE_HOST_SVC_DOWNTIME", &[Host, Int, Int, Bool, Int, Int, Text, Text]),
    ("STOP_EXECUTING_SVC_CHECKS", &[]),
    ("START_EXECUTING_SVC_CHECKS", &[]),
    ("STOP_EXECUTING_HOST_CHECKS", &[]),
    ("START_EXECUTING_HOST_CHECKS", &[]),
    ("SEND_CUSTOM_HOST_NOTIFICATION", &[Host, Int, Text, Text]),
    ("SEND_CUSTOM_SVC_NOTIFICATION", &[Service, Int, Text, Text]),
];

#[derive(Clone, Copy)]
enum Switch {
    Notifications,
    ActiveChecks,
    PassiveChecks,
    EventHandler,
    FlapDetection,
}

/// One parsed argument.
#[derive(Clone, Debug)]
enum Value {
    Obj(Obj),
    Group(Vec<Obj>),
    Int(i64),
    Text(String),
}

impl Value {
    fn obj(&self) -> Obj {
        match self {
            Value::Obj(obj) => *obj,
            _ => unreachable!(),
        }
    }
    fn int(&self) -> i64 {
        match self {
            Value::Int(n) => *n,
            _ => 0,
        }
    }
    fn text(&self) -> &str {
        match self {
            Value::Text(text) => text,
            _ => "",
        }
    }
    fn group(&self) -> Vec<Obj> {
        match self {
            Value::Group(members) => members.clone(),
            _ => Vec::new(),
        }
    }
}

impl Engine {
    /// Takes one line of the command file.
    pub fn command(&mut self, line: &str, now: Ms, out: &mut Vec<Effect>) {
        if !self.settings.check_external_commands {
            return;
        }
        let line = line.trim_end_matches(['\n', '\r']);
        let Some(rest) = line.strip_prefix('[') else {
            return;
        };
        let Some((stamp, text)) = rest.split_once(']') else {
            return;
        };
        let text = text.trim_start();
        let stamp = stamp.trim();
        let (name, args) = text.split_once(';').unwrap_or((text, ""));

        let parse_error = |why: String, out: &mut Vec<Effect>| {
            out.push(Effect::Log(format!(
                "Warning: External command parse error [{stamp}] {text} ({why})"
            )));
            out.push(Effect::Log(format!("External command error: {why}")));
        };
        let Some(&(_, kinds)) = COMMANDS.iter().find(|(known, _)| *known == name) else {
            parse_error(format!("Unknown command '{name}'"), out);
            return;
        };
        let values = match self.parse_args(kinds, args) {
            Ok(values) => values,
            Err(why) => {
                parse_error(why, out);
                return;
            }
        };
        if self.settings.log_external_commands {
            // The reference writes the `;` after the name even when no
            // arguments follow.
            out.push(Effect::Log(format!("EXTERNAL COMMAND: {name};{args}")));
        }
        let stamp: i64 = stamp.parse().unwrap_or(now.div_euclid(1000));
        if !self.carry_out(name, &values, stamp * 1000, now, out) {
            out.push(Effect::Log(format!("Error: External command failed -> {text}")));
        }
    }

    fn parse_args(&self, kinds: &[Arg], args: &str) -> Result<Vec<Value>, String> {
        let wanted = kinds.len();
        let mut fields: Vec<&str> = if args.is_empty() && wanted == 0 {
            Vec::new()
        } else {
            args.split(';').collect()
        };
        let mut values = Vec::new();
        let mut at = 0;
        for (n, kind) in kinds.iter().enumerate() {
            let last = n + 1 == wanted;
            let found = || {
                // Arguments given, with a service counted once.
                let mut count = 0;
                let mut i = 0;
                for kind in kinds {
                    if i >= fields.len() {
                        break;
                    }
                    i += if *kind == Service { 2 } else { 1 };
                    count += 1;
                }
                count
            };
            if at >= fields.len() || (*kind == Service && at + 1 >= fields.len()) {
                return Err(format!("Missing argument: expected {wanted} arguments, found {}", found()));
            }
            let value = match kind {
                Host => {
                    let name = fields[at];
                    at += 1;
                    match self.host(name) {
                        Some(obj) => Value::Obj(obj),
                        None => return Err(format!("Failed validation of host as type host (argument {n})")),
                    }
                }
                Service => {
                    let (host, description) = (fields[at], fields[at + 1]);
                    at += 2;
                    match self.service(host, description) {
                        Some(obj) => Value::Obj(obj),
                        None => {
                            return Err(format!("Failed validation of service as type service (argument {n})"));
                        }
                    }
                }
                HostGroup => {
                    let name = fields[at];
                    at += 1;
                    let Some(group) = self.objects.hostgroups.get(name) else {
                        return Err(format!("Failed validation of hostgroup as type hostgroup (argument {n})"));
                    };
                    let mut members: Vec<Obj> = group.members.iter().filter_map(|h| self.host(h)).collect();
                    members.sort();
                    Value::Group(members)
                }
                ServiceGroup => {
                    let name = fields[at];
                    at += 1;
                    let Some(group) = self.objects.servicegroups.get(name) else {
                        return Err(format!("Failed validation of servicegroup as type servicegroup (argument {n})"));
                    };
                    let members: Vec<Obj> = group.members.iter().filter_map(|(h, d)| self.service(h, d)).collect();
                    Value::Group(members)
                }
                Interval(label) => {
                    let text = fields[at].trim();
                    at += 1;
                    match text.parse::<f64>() {
                        Ok(number) => Value::Int(number as i64),
                        Err(_) => {
                            return Err(format!(
                                "Couldn't parse timestamp argument {label} (argument {n}): No digits found in ulong '{text}'"
                            ));
                        }
                    }
                }
                Word => {
                    let value = fields[at].to_string();
                    at += 1;
                    Value::Text(value)
                }
                Int | Bool => {
                    let text = fields[at].trim();
                    at += 1;
                    match text.parse::<i64>() {
                        Ok(number) => Value::Int(number),
                        Err(_) => {
                            let kind = if *kind == Int { "int" } else { "bool" };
                            return Err(format!("Failed validation of argument as type {kind} (argument {n})"));
                        }
                    }
                }
                Text => {
                    let value = if last { fields[at..].join(";") } else { fields[at].to_string() };
                    at = if last { fields.len() } else { at + 1 };
                    Value::Text(value)
                }
            };
            values.push(value);
        }
        fields.clear();
        Ok(values)
    }

    /// Turns one of an object's switches on or off, and marks it changed
    /// when that changes its value: as the reference does, a switch that is
    /// set to what it is already is not marked.
    fn switch(&mut self, obj: Obj, what: Switch, on: bool) {
        let item = self.item_mut(obj);
        let (value, bit) = match what {
            Switch::Notifications => (&mut item.cfg.notifications_enabled, MOD_NOTIFICATIONS),
            Switch::ActiveChecks => (&mut item.cfg.active_checks_enabled, MOD_ACTIVE_CHECKS),
            Switch::PassiveChecks => (&mut item.cfg.passive_checks_enabled, MOD_PASSIVE_CHECKS),
            Switch::EventHandler => (&mut item.cfg.event_handler_enabled, MOD_EVENT_HANDLER),
            Switch::FlapDetection => (&mut item.cfg.flap_detection_enabled, MOD_FLAP_DETECTION),
        };
        if *value != on {
            *value = on;
            item.modified |= bit;
        }
        if matches!(what, Switch::ActiveChecks) && on && self.item(obj).next_check.is_none() {
            let at = self.item(obj).last_check.max(self.start);
            self.check_at(obj, at, false);
        }
    }

    /// Carries out a command. False when it failed.
    fn carry_out(&mut self, name: &str, v: &[Value], stamp: Ms, now: Ms, out: &mut Vec<Effect>) -> bool {
        match name {
            "PROCESS_SERVICE_CHECK_RESULT" | "PROCESS_HOST_CHECK_RESULT" => {
                self.passive_result(v[0].obj(), v[1].int(), v[2].text(), stamp, now, out)
            }
            "ACKNOWLEDGE_HOST_PROBLEM" | "ACKNOWLEDGE_SVC_PROBLEM" => {
                let obj = v[0].obj();
                self.acknowledge(
                    obj,
                    v[1].int() > 0,
                    v[2].int() != 0,
                    v[3].int() != 0,
                    v[4].text(),
                    v[5].text(),
                    now,
                    out,
                );
                true
            }
            "REMOVE_HOST_ACKNOWLEDGEMENT" | "REMOVE_SVC_ACKNOWLEDGEMENT" => {
                self.remove_ack(v[0].obj());
                true
            }
            "SCHEDULE_HOST_DOWNTIME" | "SCHEDULE_SVC_DOWNTIME" => self
                .schedule_downtime(
                    v[0].obj(),
                    v[1].int(),
                    v[2].int(),
                    v[3].int() != 0,
                    v[4].int().max(0) as u64,
                    v[5].int(),
                    v[6].text(),
                    v[7].text(),
                    now,
                    out,
                )
                .is_ok(),
            "DEL_HOST_DOWNTIME" | "DEL_SVC_DOWNTIME" => {
                let id = v[0].int().max(0) as u64;
                let host_command = name == "DEL_HOST_DOWNTIME";
                match self.downtimes.get(&id) {
                    Some(downtime) if matches!(downtime.obj, Obj::Host(_)) == host_command => {
                        self.stop_downtime(id, true, now, out)
                    }
                    _ => false,
                }
            }
            "ADD_HOST_COMMENT" | "ADD_SVC_COMMENT" => {
                self.add_comment(v[0].obj(), 1, v[1].int() != 0, v[2].text(), v[3].text(), now, None);
                true
            }
            "DEL_HOST_COMMENT" | "DEL_SVC_COMMENT" => {
                let id = v[0].int().max(0) as u64;
                self.comments.remove(&id).is_some()
            }
            "ENABLE_HOST_NOTIFICATIONS" | "ENABLE_SVC_NOTIFICATIONS" | "DISABLE_HOST_NOTIFICATIONS"
            | "DISABLE_SVC_NOTIFICATIONS" => {
                self.switch(v[0].obj(), Switch::Notifications, name.starts_with("ENABLE"));
                true
            }
            "ENABLE_NOTIFICATIONS" => {
                self.settings.enable_notifications = true;
                true
            }
            "DISABLE_NOTIFICATIONS" => {
                self.settings.enable_notifications = false;
                true
            }
            "SCHEDULE_SVC_CHECK" | "SCHEDULE_FORCED_SVC_CHECK" | "SCHEDULE_HOST_CHECK"
            | "SCHEDULE_FORCED_HOST_CHECK" => {
                self.check_at(v[0].obj(), v[1].int() * 1000, name.contains("FORCED"));
                true
            }
            "SCHEDULE_HOST_SVC_CHECKS" | "SCHEDULE_FORCED_HOST_SVC_CHECKS" => {
                if let Obj::Host(at) = v[0].obj() {
                    for service in self.hosts[at].services.clone() {
                        self.check_at(Obj::Service(service), v[1].int() * 1000, name.contains("FORCED"));
                    }
                }
                true
            }
            "ENABLE_SVC_CHECK" | "ENABLE_HOST_CHECK" | "DISABLE_SVC_CHECK" | "DISABLE_HOST_CHECK" => {
                self.switch(v[0].obj(), Switch::ActiveChecks, name.starts_with("ENABLE"));
                true
            }
            "ENABLE_SVC_EVENT_HANDLER" | "DISABLE_SVC_EVENT_HANDLER" | "ENABLE_HOST_EVENT_HANDLER"
            | "DISABLE_HOST_EVENT_HANDLER" => {
                self.switch(v[0].obj(), Switch::EventHandler, name.starts_with("ENABLE"));
                true
            }
            "ENABLE_SVC_FLAP_DETECTION" | "DISABLE_SVC_FLAP_DETECTION" | "ENABLE_HOST_FLAP_DETECTION"
            | "DISABLE_HOST_FLAP_DETECTION" => {
                self.switch(v[0].obj(), Switch::FlapDetection, name.starts_with("ENABLE"));
                true
            }
            "ENABLE_PASSIVE_SVC_CHECKS" | "DISABLE_PASSIVE_SVC_CHECKS" | "ENABLE_PASSIVE_HOST_CHECKS"
            | "DISABLE_PASSIVE_HOST_CHECKS" => {
                self.switch(v[0].obj(), Switch::PassiveChecks, name.starts_with("ENABLE"));
                true
            }
            "ENABLE_HOST_SVC_CHECKS" | "DISABLE_HOST_SVC_CHECKS" | "ENABLE_HOST_SVC_NOTIFICATIONS"
            | "DISABLE_HOST_SVC_NOTIFICATIONS" => {
                let what = if name.ends_with("CHECKS") { Switch::ActiveChecks } else { Switch::Notifications };
                if let Obj::Host(at) = v[0].obj() {
                    for obj in self.services_of(at) {
                        self.switch(obj, what, name.starts_with("ENABLE"));
                    }
                }
                true
            }
            _ if name.contains("GROUP_") && (name.starts_with("ENABLE_") || name.starts_with("DISABLE_")) => {
                // ENABLE_HOSTGROUP_SVC_NOTIFICATIONS and the like.
                let what = if name.ends_with("CHECKS") { Switch::ActiveChecks } else { Switch::Notifications };
                let on = name.starts_with("ENABLE");
                let services = name.contains("_SVC_");
                let mut targets: Vec<Obj> = Vec::new();
                for obj in v[0].group() {
                    match (obj, services) {
                        (Obj::Host(at), true) => targets.extend(self.services_of(at)),
                        (Obj::Service(at), false) => targets.push(Obj::Host(self.services[at].host)),
                        (other, _) => targets.push(other),
                    }
                }
                targets.dedup();
                for obj in targets {
                    self.switch(obj, what, on);
                }
                true
            }
            "ENABLE_EVENT_HANDLERS" | "DISABLE_EVENT_HANDLERS" => {
                self.settings.enable_event_handlers = name.starts_with("ENABLE");
                true
            }
            "ENABLE_FLAP_DETECTION" | "DISABLE_FLAP_DETECTION" => {
                self.settings.enable_flap_detection = name.starts_with("ENABLE");
                true
            }
            "START_ACCEPTING_PASSIVE_SVC_CHECKS" | "STOP_ACCEPTING_PASSIVE_SVC_CHECKS" => {
                self.settings.accept_passive_service_checks = name.starts_with("START");
                true
            }
            "START_ACCEPTING_PASSIVE_HOST_CHECKS" | "STOP_ACCEPTING_PASSIVE_HOST_CHECKS" => {
                self.settings.accept_passive_host_checks = name.starts_with("START");
                true
            }
            "CHANGE_NORMAL_SVC_CHECK_INTERVAL" | "CHANGE_NORMAL_HOST_CHECK_INTERVAL" => {
                let obj = v[0].obj();
                let item = self.item_mut(obj);
                item.cfg.check_interval = v[1].int() as f64;
                item.modified |= MOD_NORMAL_INTERVAL;
                self.reset_intervals(obj);
                true
            }
            "CHANGE_RETRY_SVC_CHECK_INTERVAL" | "CHANGE_RETRY_HOST_CHECK_INTERVAL" => {
                let obj = v[0].obj();
                let item = self.item_mut(obj);
                item.cfg.retry_interval = v[1].int() as f64;
                item.modified |= MOD_RETRY_INTERVAL;
                self.reset_intervals(obj);
                true
            }
            "CHANGE_MAX_SVC_CHECK_ATTEMPTS" | "CHANGE_MAX_HOST_CHECK_ATTEMPTS" => {
                let item = self.item_mut(v[0].obj());
                item.cfg.max_check_attempts = v[1].int().max(1) as u32;
                item.modified |= MOD_MAX_ATTEMPTS;
                true
            }
            "CHANGE_CUSTOM_SVC_VAR" | "CHANGE_CUSTOM_HOST_VAR" => {
                // Only a variable the object has is changed; the command
                // counts as a change all the same.
                let item = self.item_mut(v[0].obj());
                let wanted = v[1].text().trim_start_matches('_').to_string();
                if let Some(var) = item.cfg.custom.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(&wanted)) {
                    var.1 = v[2].text().to_string();
                }
                item.modified |= MOD_CUSTOM_VARIABLE;
                true
            }
            "DEL_ALL_HOST_COMMENTS" | "DEL_ALL_SVC_COMMENTS" => {
                let obj = v[0].obj();
                self.comments.retain(|_, c| c.obj != obj);
                true
            }
            "SCHEDULE_HOSTGROUP_HOST_DOWNTIME" | "SCHEDULE_HOSTGROUP_SVC_DOWNTIME" | "SCHEDULE_SERVICEGROUP_SVC_DOWNTIME"
            | "SCHEDULE_SERVICEGROUP_HOST_DOWNTIME" | "SCHEDULE_HOST_SVC_DOWNTIME" => {
                let services = name.contains("_SVC_DOWNTIME");
                let members = match &v[0] {
                    Value::Obj(obj) => vec![*obj],
                    _ => v[0].group(),
                };
                let mut targets: Vec<Obj> = Vec::new();
                for obj in members {
                    match (obj, services) {
                        (Obj::Host(at), true) => targets.extend(self.services_of(at)),
                        (Obj::Service(at), false) => {
                            let host = Obj::Host(self.services[at].host);
                            if !targets.contains(&host) {
                                targets.push(host);
                            }
                        }
                        (other, _) => targets.push(other),
                    }
                }
                let mut all = true;
                for obj in targets {
                    all &= self
                        .schedule_downtime(
                            obj,
                            v[1].int(),
                            v[2].int(),
                            v[3].int() != 0,
                            v[4].int().max(0) as u64,
                            v[5].int(),
                            v[6].text(),
                            v[7].text(),
                            now,
                            out,
                        )
                        .is_ok();
                }
                all
            }
            "STOP_EXECUTING_SVC_CHECKS" | "START_EXECUTING_SVC_CHECKS" => {
                self.settings.execute_service_checks = name.starts_with("START");
                true
            }
            "STOP_EXECUTING_HOST_CHECKS" | "START_EXECUTING_HOST_CHECKS" => {
                self.settings.execute_host_checks = name.starts_with("START");
                true
            }
            "SEND_CUSTOM_HOST_NOTIFICATION" | "SEND_CUSTOM_SVC_NOTIFICATION" => {
                self.notify(v[0].obj(), Kind::Custom, v[2].text(), v[3].text(), now, out);
                true
            }
            _ => false,
        }
    }
}
