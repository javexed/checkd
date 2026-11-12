//! What a query's `AuthUser` may see.
//!
//! The rules are the reference's, read off its answers to the queries of
//! `runs/scenarios/livestatus-authuser`, with its Livestatus module's
//! defaults (`service_authorization=loose`, `group_authorization=strict`):
//!
//! - a host: its contacts, named or through a contact group;
//! - a service: its contacts, and its host's;
//! - a host group or service group: who sees every member. The tables
//!   that list members by group (`hostsbygroup` and the like) give each
//!   member the user sees, whatever the group;
//! - a comment, a downtime, a span of the state history: who sees its
//!   host or service;
//! - a line of the log: who sees the service it names, or else the host.
//!   A line that names no host is given to everyone. So is one that names
//!   a host there is none of, unless it is an alert, a notification, a
//!   passive check or a state line: those of a host that was taken out
//!   of the configuration are given to nobody who is limited (seen on a
//!   site that lost a host; the scenario cannot take a host away);
//! - contacts, contact groups, commands, timeperiods, status and columns
//!   are given to everyone.
//!
//! A user who is no contact sees nothing of the limited tables.

use checkd_core::engine::{Engine, Obj};

use crate::tables::{self, Row, Source};

fn is_contact(engine: &Engine, obj: Obj, user: &str) -> bool {
    let cfg = &engine.item(obj).cfg;
    cfg.contacts.iter().any(|c| c == user)
        || cfg
            .contact_groups
            .iter()
            .any(|g| engine.objects().contactgroups.get(g).is_some_and(|g| g.members.iter().any(|m| m == user)))
}

fn sees_host(engine: &Engine, at: usize, user: &str) -> bool {
    is_contact(engine, Obj::Host(at), user)
}

fn sees_service(engine: &Engine, at: usize, user: &str) -> bool {
    is_contact(engine, Obj::Service(at), user) || sees_host(engine, engine.services()[at].host, user)
}

fn sees(engine: &Engine, obj: Obj, user: &str) -> bool {
    match obj {
        Obj::Host(at) => sees_host(engine, at, user),
        Obj::Service(at) => sees_service(engine, at, user),
    }
}

/// By names, as the log and the state history have them: the service
/// when there is one of that name, or else the host. A host there is
/// none of is everyone's.
fn sees_named(engine: &Engine, host: &str, service: &str, user: &str) -> bool {
    if host.is_empty() {
        return true;
    }
    if !service.is_empty() {
        if let Some(obj) = engine.service(host, service) {
            return sees(engine, obj, user);
        }
    }
    match engine.host(host) {
        Some(obj) => sees(engine, obj, user),
        None => true,
    }
}

/// Whether the user may see a row.
pub fn visible(source: &Source, row: &Row, user: &str) -> bool {
    let engine = source.engine;
    let objects = engine.objects();
    match row {
        Row::Host(at) | Row::HostInGroup(at, _) => sees_host(engine, *at, user),
        Row::Service(at) | Row::ServiceInGroup(at, _) | Row::ServiceInHostGroup(at, _) => sees_service(engine, *at, user),
        Row::HostGroup(name) => objects.hostgroups.get(name).is_some_and(|group| {
            group.members.iter().all(|host| engine.host(host).is_none_or(|obj| sees(engine, obj, user)))
        }),
        Row::ServiceGroup(name) => objects.servicegroups.get(name).is_some_and(|group| {
            group.members.iter().all(|(host, service)| engine.service(host, service).is_none_or(|obj| sees(engine, obj, user)))
        }),
        Row::Comment(id) => engine.comments().find(|c| c.id == *id).is_some_and(|c| sees(engine, c.obj, user)),
        Row::Downtime(id) => engine.downtimes().find(|d| d.id == *id).is_some_and(|d| sees(engine, d.obj, user)),
        Row::StateSpan(span) => sees_named(engine, &span.host, &span.service, user),
        Row::Log(_) => {
            let text = |column: &str| match tables::get(source, row, column) {
                Some(value) => value.text(),
                None => String::new(),
            };
            let (host, service) = (text("host_name"), text("service_description"));
            // Alerts (1), notifications (3), passive checks (4) and state
            // lines (6) are about an object: of a host that is no longer
            // there, nobody who is limited sees them.
            let about_object = matches!(text("class").as_str(), "1" | "3" | "4" | "6");
            if about_object && !host.is_empty() && engine.host(&host).is_none() {
                return false;
            }
            sees_named(engine, &host, &service, user)
        }
        Row::Status | Row::ContactGroup(_) | Row::Contact(_) | Row::Command(_) | Row::Timeperiod(_) | Row::Column(..) => true,
    }
}
