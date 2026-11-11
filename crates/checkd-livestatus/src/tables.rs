//! The tables: their rows, and the value of each column in a row.
//!
//! The columns, their types and how their values are written were taken
//! from the reference's answers to queries put to it on configs written
//! for the purpose. Only the columns that Thruk was seen to ask for, and a
//! few that go with them, are here. Asking for any other is an error, as
//! it is for a column the reference does not have.

use checkd_core::engine::{Engine, Item, Ms, Obj};
use checkd_core::notify::{Comment, Downtime};

use crate::value::Value;

/// What an answer is made from: the engine, the time, and the log.
#[derive(Clone, Copy)]
pub struct Source<'a> {
    pub engine: &'a Engine,
    pub now: Ms,
    /// The log, oldest first: the second and the line.
    pub log: &'a [(i64, String)],
    /// Facts of the program that runs the engine.
    pub program: &'a Program,
    /// For `statehist`: the time the query's filters ask about.
    pub window: Option<crate::statehist::Window>,
}

/// Facts of the program, for the `status` table.
#[derive(Clone, Debug, Default)]
pub struct Program {
    pub pid: u32,
    pub version: String,
    pub livestatus_version: String,
    pub requests: u64,
    pub connections: u64,
}

/// The names of the attributes a command can change, in the reference's
/// order.
const MODIFIED: &[(u32, &str)] = &[
    (1, "notifications_enabled"),
    (2, "active_checks_enabled"),
    (4, "passive_checks_enabled"),
    (8, "event_handler_enabled"),
    (16, "flap_detection_enabled"),
    (1024, "normal_check_interval"),
    (2048, "retry_check_interval"),
    (4096, "max_check_attempts"),
    (32768, "custom_variable"),
];

#[derive(Clone, Debug)]
pub enum Row {
    Host(usize),
    Service(usize),
    /// A host in one of its host groups.
    HostInGroup(usize, String),
    /// A service in one of its service groups, or in one of its host's
    /// host groups.
    ServiceInGroup(usize, String),
    ServiceInHostGroup(usize, String),
    /// A span of one state of a host (service `None`) or service.
    StateSpan(Span),
    Status,
    HostGroup(String),
    ServiceGroup(String),
    ContactGroup(String),
    Contact(String),
    Command(String),
    Timeperiod(String),
    Comment(u64),
    Downtime(u64),
    Log(usize),
    Column(&'static str, &'static str, &'static str),
}

pub const TABLES: &[&str] = &[
    "hosts",
    "services",
    "status",
    "hostgroups",
    "servicegroups",
    "contactgroups",
    "contacts",
    "commands",
    "timeperiods",
    "comments",
    "downtimes",
    "log",
    "columns",
    "hostsbygroup",
    "servicesbygroup",
    "servicesbyhostgroup",
    "statehist",
];

/// The rows of a table, in the order the reference gives them.
pub fn rows(source: &Source, table: &str) -> Option<Vec<Row>> {
    let engine = source.engine;
    let objects = engine.objects();
    Some(match table {
        "hosts" => {
            let mut rows: Vec<usize> = (0..engine.hosts().len()).collect();
            rows.sort_by(|a, b| engine.hosts()[*a].name.cmp(&engine.hosts()[*b].name));
            rows.into_iter().map(Row::Host).collect()
        }
        "services" => {
            let mut rows: Vec<usize> = (0..engine.services().len()).collect();
            let key = |at: &usize| {
                let s = &engine.services()[*at];
                (s.host_name.clone(), s.description.clone())
            };
            rows.sort_by_key(key);
            rows.into_iter().map(Row::Service).collect()
        }
        "status" => vec![Row::Status],
        "hostgroups" => objects.hostgroups.keys().cloned().map(Row::HostGroup).collect(),
        "servicegroups" => objects.servicegroups.keys().cloned().map(Row::ServiceGroup).collect(),
        "contactgroups" => objects.contactgroups.keys().cloned().map(Row::ContactGroup).collect(),
        "contacts" => objects.contacts.keys().cloned().map(Row::Contact).collect(),
        "commands" => objects.commands.keys().cloned().map(Row::Command).collect(),
        "timeperiods" => objects.timeperiods.keys().cloned().map(Row::Timeperiod).collect(),
        "comments" => engine.comments().map(|c| Row::Comment(c.id)).collect(),
        "downtimes" => engine.downtimes().map(|d| Row::Downtime(d.id)).collect(),
        // Newest first.
        "log" => (0..source.log.len()).rev().map(Row::Log).collect(),
        "columns" => COLUMNS.iter().map(|&(t, n, ty)| Row::Column(t, n, ty)).collect(),
        "hostsbygroup" => {
            let mut out = Vec::new();
            for (name, group) in &objects.hostgroups {
                let mut members: Vec<usize> = group.members.iter().filter_map(|h| index_of_host(engine, h)).collect();
                members.sort_by(|a, b| engine.hosts()[*a].name.cmp(&engine.hosts()[*b].name));
                out.extend(members.into_iter().map(|at| Row::HostInGroup(at, name.clone())));
            }
            out
        }
        "servicesbygroup" => {
            let mut out = Vec::new();
            for (name, group) in &objects.servicegroups {
                let mut members: Vec<usize> = group
                    .members
                    .iter()
                    .filter_map(|(h, d)| engine.services().iter().position(|s| &s.host_name == h && &s.description == d))
                    .collect();
                members.sort_by_key(|at| (engine.services()[*at].host_name.clone(), engine.services()[*at].description.clone()));
                out.extend(members.into_iter().map(|at| Row::ServiceInGroup(at, name.clone())));
            }
            out
        }
        "servicesbyhostgroup" => {
            let mut out = Vec::new();
            for (name, group) in &objects.hostgroups {
                let mut hosts: Vec<usize> = group.members.iter().filter_map(|h| index_of_host(engine, h)).collect();
                hosts.sort_by(|a, b| engine.hosts()[*a].name.cmp(&engine.hosts()[*b].name));
                for host in hosts {
                    // A host's own services: in the reverse order of names.
                    let mut services = engine.hosts()[host].services.clone();
                    services.sort_by(|a, b| engine.services()[*b].description.cmp(&engine.services()[*a].description));
                    out.extend(services.into_iter().map(|at| Row::ServiceInHostGroup(at, name.clone())));
                }
            }
            out
        }
        "statehist" => crate::statehist::spans(source, &crate::statehist::Window::all())
            .into_iter()
            .map(Row::StateSpan)
            .collect(),
        _ => return None,
    })
}

pub use crate::columns::COLUMNS;

fn s(ms: Ms) -> Value {
    Value::Int(ms.div_euclid(1000))
}

/// When an object was last found up or OK. Before that was ever seen,
/// the reference gives the start of the program: the object counts as up
/// or OK from there.
fn last_ok(source: &Source, item: &Item) -> Value {
    if item.last_time[0] > 0 {
        s(item.last_time[0])
    } else {
        s(source.engine.start_time())
    }
}

fn opt(text: &Option<String>) -> Value {
    Value::str(text.clone().unwrap_or_default())
}

/// The value of a column in a row. `None` when the table has no such
/// column.
pub fn get(source: &Source, row: &Row, column: &str) -> Option<Value> {
    match row {
        Row::Host(at) => host(source, *at, column),
        Row::Service(at) => service(source, *at, column),
        Row::Status => status(source, column),
        Row::HostGroup(name) => hostgroup(source, name, column),
        Row::ServiceGroup(name) => servicegroup(source, name, column),
        Row::ContactGroup(name) => {
            let group = source.engine.objects().contactgroups.get(name)?;
            Some(match column {
                "name" => Value::str(&group.name),
                "alias" => Value::str(&group.alias),
                "members" => Value::strs(group.members.iter().cloned()),
                _ => return None,
            })
        }
        Row::Contact(name) => contact(source, name, column),
        Row::Command(name) => {
            let command = source.engine.objects().commands.get(name)?;
            Some(match column {
                "name" => Value::str(&command.name),
                "line" => Value::str(&command.line),
                _ => return None,
            })
        }
        Row::Timeperiod(name) => {
            let period = source.engine.objects().timeperiods.get(name)?;
            let second = source.now.div_euclid(1000);
            Some(match column {
                "name" => Value::str(&period.name),
                "alias" => Value::str(&period.alias),
                "exclusions" => Value::strs(period.exclude.iter().cloned()),
                "in" => Value::bool(source.engine.in_period(Some(name), second)),
                _ => return None,
            })
        }
        Row::Comment(id) => {
            let comment = source.engine.comments().find(|c| c.id == *id)?;
            comment_column(source, comment, column)
        }
        Row::Downtime(id) => {
            let downtime = source.engine.downtimes().find(|d| d.id == *id)?;
            downtime_column(source, downtime, column)
        }
        Row::Log(at) => log_column(source, *at, column),
        Row::HostInGroup(at, group) => match column.strip_prefix("hostgroup_") {
            Some(rest) => hostgroup(source, group, rest),
            None => host(source, *at, column),
        },
        Row::ServiceInGroup(at, group) => match column.strip_prefix("servicegroup_") {
            Some(rest) => servicegroup(source, group, rest),
            None => service(source, *at, column),
        },
        Row::ServiceInHostGroup(at, group) => match column.strip_prefix("hostgroup_") {
            Some(rest) => hostgroup(source, group, rest),
            None => service(source, *at, column),
        },
        Row::StateSpan(span) => crate::statehist::column(source, span, column),
        Row::Column(table, name, ty) => Some(match column {
            "table" => Value::str(*table),
            "name" => Value::str(*name),
            "type" => Value::str(*ty),
            "description" => Value::str(""),
            _ => return None,
        }),
    }
}

fn index_of_host(engine: &Engine, name: &str) -> Option<usize> {
    engine.hosts().iter().position(|h| h.name == name)
}

pub use crate::statehist::Span;

fn names(engine: &Engine, obj: Obj) -> (String, String) {
    match obj {
        Obj::Host(at) => (engine.hosts()[at].name.clone(), String::new()),
        Obj::Service(at) => {
            let s = &engine.services()[at];
            (s.host_name.clone(), s.description.clone())
        }
    }
}

fn comment_ids(engine: &Engine, obj: Obj) -> Vec<&Comment> {
    let mut list: Vec<&Comment> = engine.comments().filter(|c| c.obj == obj).collect();
    list.sort_by_key(|c| c.id);
    list
}

fn downtime_list(engine: &Engine, obj: Obj) -> Vec<&Downtime> {
    let mut list: Vec<&Downtime> = engine.downtimes().filter(|d| d.obj == obj).collect();
    list.sort_by_key(|d| d.id);
    list
}

/// Columns hosts and services have in common.
fn common(source: &Source, obj: Obj, item: &Item, column: &str) -> Option<Value> {
    let engine = source.engine;
    let cfg = &item.cfg;
    let second = source.now.div_euclid(1000);
    let expand = |text: &Option<String>| Value::str(engine.expand(text.as_deref().unwrap_or(""), obj, source.now));
    Some(match column {
        "accept_passive_checks" => Value::bool(cfg.passive_checks_enabled),
        "acknowledged" => Value::bool(item.notice.ack.is_some()),
        "acknowledgement_type" => Value::Int(match &item.notice.ack {
            Some(ack) if ack.sticky => 2,
            Some(_) => 1,
            None => 0,
        }),
        "action_url" => opt(&cfg.action_url),
        "action_url_expanded" => expand(&cfg.action_url),
        "active_checks_enabled" | "checks_enabled" => Value::bool(cfg.active_checks_enabled),
        "check_command" => opt(&cfg.check_command),
        "check_freshness" => Value::bool(cfg.check_freshness),
        "check_interval" => Value::Float(cfg.check_interval),
        "check_options" => Value::Int(0),
        "check_period" => opt(&cfg.check_period),
        "check_type" => Value::bool(item.passive_result),
        "comments" => Value::List(comment_ids(engine, obj).iter().map(|c| Value::Int(c.id as i64)).collect()),
        "comments_with_info" => Value::List(
            comment_ids(engine, obj)
                .iter()
                .map(|c| {
                    Value::List(vec![
                        Value::Int(c.id as i64),
                        Value::str(&c.author),
                        Value::str(&c.text),
                        Value::Int(c.entry),
                        Value::Int(c.entry_type as i64),
                        Value::bool(c.expires.is_some()),
                        Value::Int(c.expires.unwrap_or(0)),
                    ])
                })
                .collect(),
        ),
        "contact_groups" => Value::strs(cfg.contact_groups.iter().cloned()),
        "contacts" => {
            let mut all: Vec<String> = cfg.contacts.clone();
            for group in &cfg.contact_groups {
                if let Some(group) = engine.objects().contactgroups.get(group) {
                    all.extend(group.members.iter().cloned());
                }
            }
            all.sort();
            all.dedup();
            Value::strs(all)
        }
        "current_attempt" => Value::Int(item.attempt as i64),
        "current_notification_number" => Value::Int(item.notice.number as i64),
        "custom_variable_names" => Value::strs(cfg.custom.iter().map(|(n, _)| n.clone())),
        "custom_variable_values" => Value::strs(cfg.custom.iter().map(|(_, v)| v.clone())),
        "custom_variables" => Value::Dict(cfg.custom.clone()),
        "depends_exec" | "depends_notify" => Value::List(Vec::new()),
        "display_name" => Value::str(cfg.display_name.clone().unwrap_or_else(|| match obj {
            Obj::Host(at) => engine.hosts()[at].name.clone(),
            Obj::Service(at) => engine.services()[at].description.clone(),
        })),
        "downtimes" => Value::List(downtime_list(engine, obj).iter().map(|d| Value::Int(d.id as i64)).collect()),
        "downtimes_with_info" => Value::List(
            downtime_list(engine, obj)
                .iter()
                .map(|d| {
                    Value::List(vec![
                        Value::Int(d.id as i64),
                        Value::str(&d.author),
                        Value::str(&d.comment),
                        Value::Int(d.entry),
                        Value::Int(d.start),
                        Value::Int(d.end),
                        Value::bool(d.fixed),
                        Value::Int(if d.fixed { d.end - d.start } else { d.duration }),
                        Value::Int(d.trigger as i64),
                    ])
                })
                .collect(),
        ),
        "event_handler" => opt(&cfg.event_handler),
        "event_handler_enabled" => Value::bool(cfg.event_handler_enabled),
        "execution_time" => Value::Float(item.execution_time),
        "first_notification_delay" => Value::Float(cfg.first_notification_delay),
        "flap_detection_enabled" => Value::bool(cfg.flap_detection_enabled),
        "has_been_checked" => Value::bool(item.checked),
        "high_flap_threshold" => Value::Float(cfg.high_flap_threshold),
        "icon_image" => opt(&cfg.icon_image),
        "icon_image_alt" => opt(&cfg.icon_image_alt),
        "icon_image_expanded" => expand(&cfg.icon_image),
        "in_check_period" => Value::bool(engine.in_period(cfg.check_period.as_deref(), second)),
        "in_notification_period" => Value::bool(engine.in_period(cfg.notification_period.as_deref(), second)),
        "is_executing" => Value::bool(item.running),
        "is_flapping" => Value::bool(item.flapping),
        "last_check" => s(item.last_check),
        "last_hard_state" => Value::Int(item.last_hard_state as i64),
        "last_hard_state_change" => s(item.last_hard_state_change),
        "last_notification" => s(item.notice.last),
        "last_state" => Value::Int(item.last_state as i64),
        "last_state_change" => s(item.last_state_change),
        "last_update" => Value::Time(item.last_update as f64 / 1000.0),
        "latency" => Value::Float(item.latency),
        "long_plugin_output" => Value::str(&item.output.long),
        "low_flap_threshold" => Value::Float(cfg.low_flap_threshold),
        "max_check_attempts" => Value::Int(cfg.max_check_attempts as i64),
        "modified_attributes" => Value::Int(item.modified as i64),
        "modified_attributes_list" => Value::strs(
            MODIFIED
                .iter()
                .filter(|(bit, _)| item.modified & bit != 0)
                .map(|(_, name)| name.to_string()),
        ),
        "next_check" => s(item.next_check.unwrap_or(0)),
        "notes" => opt(&cfg.notes),
        "notes_expanded" => expand(&cfg.notes),
        "notes_url" => opt(&cfg.notes_url),
        "notes_url_expanded" => expand(&cfg.notes_url),
        "notification_interval" => Value::Float(cfg.notification_interval),
        "notification_period" => opt(&cfg.notification_period),
        "notifications_enabled" => Value::bool(cfg.notifications_enabled),
        "percent_state_change" => Value::Float(item.percent_change),
        "perf_data" => Value::str(&item.output.perfdata),
        "plugin_output" => Value::str(&item.output.short),
        "process_performance_data" => Value::bool(cfg.process_perf_data),
        "retry_interval" => Value::Float(cfg.retry_interval),
        "scheduled_downtime_depth" => Value::Int(item.notice.downtime_depth as i64),
        "state" => Value::Int(item.state as i64),
        "state_type" => Value::bool(item.hard),
        _ => return None,
    })
}

fn host(source: &Source, at: usize, column: &str) -> Option<Value> {
    let engine = source.engine;
    let entry = &engine.hosts()[at];
    let item = &entry.item;
    let cfg_host = engine.objects().hosts.get(&entry.name)?;
    let services = || -> Vec<&checkd_core::engine::ServiceEntry> {
        // The reference keeps them in the reverse order of the names.
        let mut list: Vec<_> = entry.services.iter().map(|&s| &engine.services()[s]).collect();
        list.sort_by(|a, b| b.description.cmp(&a.description));
        list
    };
    let count = |state: Option<u8>, pending: bool| {
        Value::Int(
            services()
                .iter()
                .filter(|s| {
                    if pending {
                        !s.item.checked
                    } else {
                        s.item.checked && state.is_none_or(|st| s.item.state == st)
                    }
                })
                .count() as i64,
        )
    };
    Some(match column {
        "name" => Value::str(&entry.name),
        "alias" => Value::str(&cfg_host.alias),
        "address" => Value::str(&cfg_host.address),
        "groups" => Value::strs(entry.groups.iter().cloned()),
        "parents" => Value::strs(entry.parents.iter().map(|&p| engine.hosts()[p].name.clone())),
        "childs" => Value::strs(entry.children.iter().map(|&c| engine.hosts()[c].name.clone())),
        "hard_state" => Value::Int(if item.hard { item.state } else { item.last_hard_state } as i64),
        "last_time_up" => last_ok(source, item),
        "last_time_down" => s(item.last_time[1]),
        "last_time_unreachable" => s(item.last_time[2]),
        "obsess_over_host" => Value::bool(item.cfg.obsess),
        "num_services" => Value::Int(entry.services.len() as i64),
        "num_services_ok" => count(Some(0), false),
        "num_services_warn" => count(Some(1), false),
        "num_services_crit" => count(Some(2), false),
        "num_services_unknown" => count(Some(3), false),
        "num_services_pending" => count(None, true),
        "services" => Value::strs(services().iter().map(|s| s.description.clone())),
        "services_with_state" => Value::List(
            services()
                .iter()
                .map(|s| {
                    Value::List(vec![
                        Value::str(&s.description),
                        Value::Int(s.item.state as i64),
                        Value::bool(s.item.checked),
                    ])
                })
                .collect(),
        ),
        "services_with_info" => Value::List(
            services()
                .iter()
                .map(|s| {
                    Value::List(vec![
                        Value::str(&s.description),
                        Value::Int(s.item.state as i64),
                        Value::bool(s.item.checked),
                        Value::str(&s.item.output.short),
                    ])
                })
                .collect(),
        ),
        _ => return common(source, Obj::Host(at), item, column),
    })
}

fn service(source: &Source, at: usize, column: &str) -> Option<Value> {
    let engine = source.engine;
    let entry = &engine.services()[at];
    let item = &entry.item;
    if let Some(rest) = column.strip_prefix("host_") {
        if rest == "name" {
            return Some(Value::str(&entry.host_name));
        }
        return host(source, entry.host, rest);
    }
    Some(match column {
        "description" => Value::str(&entry.description),
        "groups" => Value::strs(entry.groups.iter().cloned()),
        "parents" => Value::List(Vec::new()),
        "last_time_ok" => last_ok(source, item),
        "last_time_warning" => s(item.last_time[1]),
        "last_time_critical" => s(item.last_time[2]),
        "last_time_unknown" => s(item.last_time[3]),
        "obsess_over_service" => Value::bool(item.cfg.obsess),
        "is_volatile" => Value::bool(entry.volatile),
        _ => return common(source, Obj::Service(at), item, column),
    })
}

fn status(source: &Source, column: &str) -> Option<Value> {
    let engine = source.engine;
    let settings = engine.settings();
    let program = source.program;
    let (host_checks, service_checks) = engine.checks_run();
    let start = engine.start_time();
    let elapsed = ((source.now - start) as f64 / 1000.0).max(1.0);
    Some(match column {
        "accept_passive_host_checks" => Value::bool(settings.accept_passive_host_checks),
        "accept_passive_service_checks" => Value::bool(settings.accept_passive_service_checks),
        "check_external_commands" => Value::bool(settings.check_external_commands),
        "check_host_freshness" => Value::bool(settings.check_host_freshness),
        "check_service_freshness" => Value::bool(settings.check_service_freshness),
        "enable_event_handlers" => Value::bool(settings.enable_event_handlers),
        "enable_flap_detection" => Value::bool(settings.enable_flap_detection),
        "enable_notifications" => Value::bool(settings.enable_notifications),
        "execute_host_checks" => Value::bool(settings.execute_host_checks),
        "execute_service_checks" => Value::bool(settings.execute_service_checks),
        "obsess_over_hosts" => Value::bool(settings.obsess_over_hosts),
        "obsess_over_services" => Value::bool(settings.obsess_over_services),
        "process_performance_data" => Value::bool(settings.process_performance_data),
        "interval_length" => Value::Int(settings.interval_length as i64),
        "program_start" => s(start),
        "program_version" => Value::str(&program.version),
        "livestatus_version" => Value::str(&program.livestatus_version),
        "nagios_pid" => Value::Int(program.pid as i64),
        "last_command_check" | "last_log_rotation" => Value::Int(0),
        "host_checks" => Value::Int(host_checks as i64),
        "host_checks_rate" => Value::Float(host_checks as f64 / elapsed),
        "service_checks" => Value::Int(service_checks as i64),
        "service_checks_rate" => Value::Float(service_checks as f64 / elapsed),
        "connections" => Value::Int(program.connections as i64),
        "connections_rate" => Value::Float(program.connections as f64 / elapsed),
        "requests" => Value::Int(program.requests as i64),
        "requests_rate" => Value::Float(program.requests as f64 / elapsed),
        "log_messages" | "cached_log_messages" => Value::Int(source.log.len() as i64),
        "log_messages_rate" => Value::Float(source.log.len() as f64 / elapsed),
        "forks" | "neb_callbacks" => Value::Int(0),
        "forks_rate" | "neb_callbacks_rate" => Value::Float(0.0),
        _ => return None,
    })
}

/// Counts over services, for the group tables.
fn service_counts(source: &Source, members: &[usize], column: &str) -> Option<Value> {
    let engine = source.engine;
    let list: Vec<&Item> = members.iter().map(|&s| &engine.services()[s].item).collect();
    let checked = |i: &&&Item| i.checked;
    let count = |f: &dyn Fn(&Item) -> bool| Value::Int(list.iter().filter(checked).filter(|i| f(i)).count() as i64);
    let hard = |i: &Item| if i.hard { i.state } else { i.last_hard_state };
    Some(match column {
        "num_services" => Value::Int(list.len() as i64),
        "num_services_ok" => count(&|i| i.state == 0),
        "num_services_warn" => count(&|i| i.state == 1),
        "num_services_crit" => count(&|i| i.state == 2),
        "num_services_unknown" => count(&|i| i.state == 3),
        "num_services_pending" => Value::Int(list.iter().filter(|i| !i.checked).count() as i64),
        "num_services_hard_ok" => count(&|i| hard(i) == 0),
        "num_services_hard_warn" => count(&|i| hard(i) == 1),
        "num_services_hard_crit" => count(&|i| hard(i) == 2),
        "num_services_hard_unknown" => count(&|i| hard(i) == 3),
        "worst_service_state" => Value::Int(list.iter().filter(checked).map(|i| worst_rank(i.state)).max().map_or(0, unrank)),
        "worst_service_hard_state" => {
            Value::Int(list.iter().filter(checked).map(|i| worst_rank(hard(i))).max().map_or(0, unrank))
        }
        _ => return None,
    })
}

/// CRITICAL counts worse than UNKNOWN, which counts worse than WARNING.
fn worst_rank(state: u8) -> u8 {
    match state {
        2 => 3,
        3 => 2,
        other => other,
    }
}

fn unrank(rank: u8) -> i64 {
    match rank {
        3 => 2,
        2 => 3,
        other => other as i64,
    }
}

fn hostgroup(source: &Source, name: &str, column: &str) -> Option<Value> {
    let engine = source.engine;
    let group = engine.objects().hostgroups.get(name)?;
    let hosts: Vec<usize> = group
        .members
        .iter()
        .filter_map(|n| engine.hosts().iter().position(|h| &h.name == n))
        .collect();
    let services: Vec<usize> = hosts.iter().flat_map(|&h| engine.hosts()[h].services.clone()).collect();
    let host_items: Vec<&Item> = hosts.iter().map(|&h| &engine.hosts()[h].item).collect();
    let count = |f: &dyn Fn(&Item) -> bool| Value::Int(host_items.iter().filter(|i| i.checked && f(i)).count() as i64);
    Some(match column {
        "name" => Value::str(&group.name),
        "alias" => Value::str(&group.alias),
        "notes" => opt(&group.notes),
        "notes_url" => opt(&group.notes_url),
        "action_url" => opt(&group.action_url),
        "members" => Value::strs(group.members.iter().cloned()),
        "num_hosts" => Value::Int(hosts.len() as i64),
        "num_hosts_up" => count(&|i| i.state == 0),
        "num_hosts_down" => count(&|i| i.state == 1),
        "num_hosts_unreach" => count(&|i| i.state == 2),
        "num_hosts_pending" => Value::Int(host_items.iter().filter(|i| !i.checked).count() as i64),
        "worst_host_state" => Value::Int(host_items.iter().filter(|i| i.checked).map(|i| i.state as i64).max().unwrap_or(0)),
        other => return service_counts(source, &services, other),
    })
}

fn servicegroup(source: &Source, name: &str, column: &str) -> Option<Value> {
    let engine = source.engine;
    let group = engine.objects().servicegroups.get(name)?;
    let services: Vec<usize> = group
        .members
        .iter()
        .filter_map(|(h, d)| engine.services().iter().position(|s| &s.host_name == h && &s.description == d))
        .collect();
    Some(match column {
        "name" => Value::str(&group.name),
        "alias" => Value::str(&group.alias),
        "notes" => opt(&group.notes),
        "notes_url" => opt(&group.notes_url),
        "action_url" => opt(&group.action_url),
        "members" => Value::List(
            group
                .members
                .iter()
                .map(|(h, d)| Value::List(vec![Value::str(h), Value::str(d)]))
                .collect(),
        ),
        other => return service_counts(source, &services, other),
    })
}

fn contact(source: &Source, name: &str, column: &str) -> Option<Value> {
    let engine = source.engine;
    let contact = engine.objects().contacts.get(name)?;
    let second = source.now.div_euclid(1000);
    let id = engine.contact_names().position(|n| n == name).unwrap_or(0);
    let groups: Vec<String> = engine
        .objects()
        .contactgroups
        .values()
        .filter(|g| g.members.contains(name))
        .map(|g| g.name.clone())
        .collect();
    Some(match column {
        "id" => Value::Int(id as i64),
        "name" => Value::str(&contact.name),
        "alias" => Value::str(&contact.alias),
        "email" => opt(&contact.email),
        "pager" => opt(&contact.pager),
        "can_submit_commands" => Value::bool(contact.can_submit_commands),
        "groups" => Value::strs(groups),
        "host_notification_period" => opt(&contact.host_notification_period),
        "service_notification_period" => opt(&contact.service_notification_period),
        "host_notification_commands" => Value::strs(contact.host_notification_commands.iter().cloned()),
        "service_notification_commands" => Value::strs(contact.service_notification_commands.iter().cloned()),
        "host_notifications_enabled" => Value::bool(contact.host_notifications_enabled),
        "service_notifications_enabled" => Value::bool(contact.service_notifications_enabled),
        "in_host_notification_period" => {
            Value::bool(engine.in_period(contact.host_notification_period.as_deref(), second))
        }
        "in_service_notification_period" => {
            Value::bool(engine.in_period(contact.service_notification_period.as_deref(), second))
        }
        "custom_variable_names" => Value::strs(contact.custom.iter().map(|(n, _)| n.clone())),
        "custom_variable_values" => Value::strs(contact.custom.iter().map(|(_, v)| v.clone())),
        "modified_attributes_list" => Value::List(Vec::new()),
        "last_update" => Value::str(""),
        _ => {
            let n: usize = column.strip_prefix("address")?.parse().ok()?;
            if !(1..=6).contains(&n) {
                return None;
            }
            Value::str(contact.addresses[n - 1].clone().unwrap_or_default())
        }
    })
}

fn comment_column(source: &Source, c: &Comment, column: &str) -> Option<Value> {
    let (host, service) = names(source.engine, c.obj);
    Some(match column {
        "id" => Value::Int(c.id as i64),
        "author" => Value::str(&c.author),
        "comment" => Value::str(&c.text),
        "entry_time" => Value::Int(c.entry),
        "entry_type" => Value::Int(c.entry_type as i64),
        "expires" => Value::bool(c.expires.is_some()),
        "expire_time" => Value::Int(c.expires.unwrap_or(0)),
        "persistent" => Value::bool(c.persistent),
        // 0 for the comment of a downtime, 1 for the others.
        "source" => Value::Int((c.entry_type != 2) as i64),
        "type" => Value::Int(if matches!(c.obj, Obj::Host(_)) { 1 } else { 2 }),
        "host_name" => Value::str(host),
        "service_description" => Value::str(service),
        "is_service" => Value::bool(matches!(c.obj, Obj::Service(_))),
        _ => return None,
    })
}

fn downtime_column(source: &Source, d: &Downtime, column: &str) -> Option<Value> {
    let (host, service) = names(source.engine, d.obj);
    Some(match column {
        "id" => Value::Int(d.id as i64),
        "author" => Value::str(&d.author),
        "comment" => Value::str(&d.comment),
        "entry_time" => Value::Int(d.entry),
        "start_time" => Value::Int(d.start),
        "end_time" => Value::Int(d.end),
        "fixed" => Value::bool(d.fixed),
        "duration" => Value::Int(if d.fixed { d.end - d.start } else { d.duration }),
        "triggered_by" => Value::Int(d.trigger as i64),
        "host_name" => Value::str(host),
        "service_description" => Value::str(service),
        "is_service" => Value::bool(matches!(d.obj, Obj::Service(_))),
        "type" => Value::Int(if matches!(d.obj, Obj::Host(_)) { 1 } else { 2 }),
        _ => return None,
    })
}

/// The kinds of line the reference's log table knows. A line of any
/// other kind keeps all of its text as its type.
const KINDS: &[&str] = &[
    "HOST ALERT",
    "SERVICE ALERT",
    "INITIAL HOST STATE",
    "INITIAL SERVICE STATE",
    "CURRENT HOST STATE",
    "CURRENT SERVICE STATE",
    "HOST NOTIFICATION",
    "SERVICE NOTIFICATION",
    "HOST NOTIFICATION SUPPRESSED",
    "SERVICE NOTIFICATION SUPPRESSED",
    "HOST DOWNTIME ALERT",
    "SERVICE DOWNTIME ALERT",
    "HOST FLAPPING ALERT",
    "SERVICE FLAPPING ALERT",
    "HOST EVENT HANDLER",
    "SERVICE EVENT HANDLER",
    "PASSIVE HOST CHECK",
    "PASSIVE SERVICE CHECK",
    "EXTERNAL COMMAND",
];

/// The parts of a log line the log table shows.
struct LogLine<'a> {
    /// The kind, when it is one of `KINDS`; otherwise empty.
    kind: &'a str,
    /// What the type column shows.
    shown: &'a str,
    options: &'a str,
    fields: Vec<&'a str>,
}

fn log_line(text: &str) -> LogLine<'_> {
    match text.split_once(": ") {
        Some((kind, options)) if KINDS.contains(&kind) => LogLine {
            kind,
            shown: kind,
            options,
            fields: options.split(';').collect(),
        },
        Some((_, options)) => LogLine {
            kind: "",
            shown: text,
            options,
            fields: Vec::new(),
        },
        None => LogLine {
            kind: "",
            shown: text,
            options: "",
            fields: Vec::new(),
        },
    }
}

/// The class of a line: 0 information, 1 alert, 2 program, 3 notification,
/// 4 passive check, 5 external command, 6 state.
fn log_class(line: &LogLine, text: &str) -> i64 {
    match line.kind {
        "HOST ALERT" | "SERVICE ALERT" | "HOST FLAPPING ALERT" | "SERVICE FLAPPING ALERT"
        | "HOST DOWNTIME ALERT" | "SERVICE DOWNTIME ALERT" => 1,
        "HOST NOTIFICATION" | "SERVICE NOTIFICATION" => 3,
        "PASSIVE HOST CHECK" | "PASSIVE SERVICE CHECK" => 4,
        "EXTERNAL COMMAND" => 5,
        "INITIAL HOST STATE" | "INITIAL SERVICE STATE" | "CURRENT HOST STATE" | "CURRENT SERVICE STATE" => 6,
        "" if text.starts_with("TIMEPERIOD TRANSITION: ") => 6,
        "" if text.starts_with("LOG VERSION: ")
            || text.contains(" starting... (PID=")
            || text.starts_with("Caught ")
            || text.starts_with("Successfully shutdown") =>
        {
            2
        }
        _ => 0,
    }
}

fn log_column(source: &Source, at: usize, column: &str) -> Option<Value> {
    let (time, text) = source.log.get(at)?;
    let line = log_line(text);
    let f = |n: usize| line.fields.get(n).copied().unwrap_or("");
    let kind = line.kind;
    let service_kind = kind.contains("SERVICE");
    let notification = kind == "HOST NOTIFICATION" || kind == "SERVICE NOTIFICATION";
    let state_line = matches!(
        kind,
        "HOST ALERT" | "SERVICE ALERT" | "INITIAL HOST STATE" | "INITIAL SERVICE STATE" | "CURRENT HOST STATE"
            | "CURRENT SERVICE STATE"
    );
    // The host and service a line is about, and where its state is.
    let (host, service, state_at) = if kind.is_empty() {
        ("", "", None)
    } else if kind == "EXTERNAL COMMAND" {
        let name = f(0);
        if name.contains("HOST_SVC") {
            // The services of a host: only the host is named.
            (f(1), "", None)
        } else if name.contains("_SVC_") || name.contains("SERVICE") {
            (f(1), f(2), None)
        } else if name.contains("HOST") {
            (f(1), "", None)
        } else {
            ("", "", None)
        }
    } else if notification {
        (f(1), if service_kind { f(2) } else { "" }, Some(if service_kind { 3 } else { 2 }))
    } else if service_kind {
        (f(0), f(1), Some(2))
    } else {
        (f(0), "", Some(1))
    };
    let state = match (state_at, state_line || notification) {
        (Some(at), true) => {
            let name = f(at);
            let base = name.split(" (").nth(if name.contains('(') { 1 } else { 0 }).unwrap_or("");
            let base = base.trim_end_matches(')');
            match (service_kind, base) {
                (false, "UP") => 0,
                (false, "DOWN") => 1,
                (false, "UNREACHABLE") => 2,
                (true, "OK") => 0,
                (true, "WARNING") => 1,
                (true, "CRITICAL") => 2,
                (true, "UNKNOWN") => 3,
                _ => 0,
            }
        }
        _ => 0,
    };
    let state_type = match (kind, state_at) {
        (_, Some(at)) if state_line => f(at + 1),
        ("HOST DOWNTIME ALERT" | "SERVICE DOWNTIME ALERT" | "HOST FLAPPING ALERT" | "SERVICE FLAPPING ALERT", Some(at)) => {
            f(at)
        }
        _ => "",
    };
    Some(match column {
        "time" => Value::Int(*time),
        "class" => Value::Int(log_class(&line, text)),
        "type" => Value::str(line.shown),
        "message" => Value::str(format!("[{time}] {text}")),
        "options" => Value::str(line.options),
        "host_name" => Value::str(host),
        "service_description" => Value::str(service),
        "contact_name" => Value::str(if notification { f(0) } else { "" }),
        "command_name" => Value::str(match (notification, state_at) {
            (true, Some(at)) => f(at + 1),
            _ => "",
        }),
        "plugin_output" => Value::str(match state_at {
            Some(at) if state_line => f(at + 3),
            Some(at) if notification => f(at + 2),
            _ => "",
        }),
        "state" => Value::Int(state),
        "state_type" => Value::str(state_type),
        "attempt" => Value::Int(match state_at {
            Some(at) if state_line => f(at + 2).parse().unwrap_or(0),
            _ => 0,
        }),
        "lineno" => Value::Int(at as i64 + 1),
        _ => return None,
    })
}
