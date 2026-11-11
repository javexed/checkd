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

use crate::engine::{Effect, Engine, Ms, Obj};
use crate::notify::Kind;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Arg {
    Host,
    Service,
    Int,
    Bool,
    /// A text that takes the rest of the line.
    Text,
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
    ("SEND_CUSTOM_HOST_NOTIFICATION", &[Host, Int, Text, Text]),
    ("SEND_CUSTOM_SVC_NOTIFICATION", &[Service, Int, Text, Text]),
];

/// One parsed argument.
#[derive(Clone, Debug)]
enum Value {
    Obj(Obj),
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
            out.push(Effect::Log(format!("EXTERNAL COMMAND: {text}")));
        }
        if !self.carry_out(name, &values, now, out) {
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

    /// Carries out a command. False when it failed.
    fn carry_out(&mut self, name: &str, v: &[Value], now: Ms, out: &mut Vec<Effect>) -> bool {
        match name {
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
            "ENABLE_HOST_NOTIFICATIONS" | "ENABLE_SVC_NOTIFICATIONS" => {
                self.item_mut(v[0].obj()).cfg.notifications_enabled = true;
                true
            }
            "DISABLE_HOST_NOTIFICATIONS" | "DISABLE_SVC_NOTIFICATIONS" => {
                self.item_mut(v[0].obj()).cfg.notifications_enabled = false;
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
            "SEND_CUSTOM_HOST_NOTIFICATION" | "SEND_CUSTOM_SVC_NOTIFICATION" => {
                self.notify(v[0].obj(), Kind::Custom, v[2].text(), v[3].text(), now, out);
                true
            }
            _ => false,
        }
    }
}
