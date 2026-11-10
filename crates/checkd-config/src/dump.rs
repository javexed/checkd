//! Writes loaded objects as text, in the layout of Naemon's precached
//! object file, so that the two can be compared.

use std::fmt::Write;

use crate::model::*;
use crate::timeperiod::WEEKDAYS;

struct Out {
    text: String,
}

impl Out {
    fn open(&mut self, kind: &str) {
        let _ = writeln!(self.text, "define {kind} {{");
    }

    fn close(&mut self) {
        self.text.push_str("\t}\n\n");
    }

    fn line(&mut self, key: &str, value: impl std::fmt::Display) {
        let _ = writeln!(self.text, "\t{key}\t{value}");
    }

    fn opt(&mut self, key: &str, value: &Option<String>) {
        if let Some(value) = value {
            self.line(key, value);
        }
    }

    fn list(&mut self, key: &str, items: &[String]) {
        if !items.is_empty() {
            self.line(key, items.join(","));
        }
    }

    fn flag(&mut self, key: &str, value: bool) {
        self.line(key, u8::from(value));
    }

    fn float(&mut self, key: &str, value: f64) {
        self.line(key, format_args!("{value:.6}"));
    }

    fn custom(&mut self, vars: &[CustomVar]) {
        for (name, value) in vars {
            self.line(&format!("_{name}"), value);
        }
    }

    /// The settings of a host or service from `check_period` on. `middle`
    /// writes what only one of the two has.
    fn checkable(&mut self, check: &Checkable, is_volatile: Option<bool>, host: Option<&Host>) {
        self.opt("check_period", &check.check_period);
        self.opt("check_command", &check.check_command);
        self.opt("event_handler", &check.event_handler);
        self.list("contacts", &check.contacts);
        self.list("contact_groups", &check.contact_groups);
        self.opt("notification_period", &check.notification_period);
        self.line("initial_state", check.initial_state);
        self.line("check_timeout", check.check_timeout);
        self.line("hourly_value", check.hourly_value);
        self.float("check_interval", check.check_interval);
        self.float("retry_interval", check.retry_interval);
        self.line("max_check_attempts", check.max_check_attempts);
        if let Some(is_volatile) = is_volatile {
            self.flag("is_volatile", is_volatile);
        }
        self.flag("active_checks_enabled", check.active_checks_enabled);
        self.flag("passive_checks_enabled", check.passive_checks_enabled);
        self.flag("obsess", check.obsess);
        self.flag("event_handler_enabled", check.event_handler_enabled);
        self.float("low_flap_threshold", check.low_flap_threshold);
        self.float("high_flap_threshold", check.high_flap_threshold);
        self.flag("flap_detection_enabled", check.flap_detection_enabled);
        self.line(
            "flap_detection_options",
            check.flap_detection_options.dump(),
        );
        self.line("freshness_threshold", check.freshness_threshold);
        self.flag("check_freshness", check.check_freshness);
        self.line("notification_options", check.notification_options.dump());
        self.flag("notifications_enabled", check.notifications_enabled);
        self.float("notification_interval", check.notification_interval);
        self.float("first_notification_delay", check.first_notification_delay);
        self.line("stalking_options", check.stalking_options.dump());
        self.flag("process_perf_data", check.process_perf_data);
        self.opt("icon_image", &check.icon_image);
        self.opt("icon_image_alt", &check.icon_image_alt);
        if let Some(host) = host {
            self.opt("vrml_image", &host.vrml_image);
            self.opt("statusmap_image", &host.statusmap_image);
            if let Some((x, y)) = host.coords_2d {
                self.line("2d_coords", format_args!("{x},{y}"));
            }
            if let Some((x, y, z)) = host.coords_3d {
                self.line("3d_coords", format_args!("{x:.6},{y:.6},{z:.6}"));
            }
        }
        self.opt("notes", &check.notes);
        self.opt("notes_url", &check.notes_url);
        self.opt("action_url", &check.action_url);
        self.flag("retain_status_information", check.retain_status_information);
        self.flag(
            "retain_nonstatus_information",
            check.retain_nonstatus_information,
        );
        self.custom(&check.custom);
    }

    fn group<M: Ord>(&mut self, kind: &str, group: &Group<M>, members: Vec<String>) {
        self.open(kind);
        self.line(&format!("{kind}_name"), &group.name);
        self.line("alias", &group.alias);
        self.list("members", &members);
        self.opt("notes", &group.notes);
        self.opt("notes_url", &group.notes_url);
        self.opt("action_url", &group.action_url);
        self.close();
    }

    /// A dependency is written once for each of its two kinds, and not at
    /// all for a kind with no failure states.
    fn dependency<Id>(
        &mut self,
        kind: &str,
        dependency: &Dependency<Id>,
        names: impl Fn(&mut Out, &Id, &str),
    ) {
        for (key, options) in [
            ("execution_failure_options", &dependency.execution_failure),
            (
                "notification_failure_options",
                &dependency.notification_failure,
            ),
        ] {
            if options.is_none() {
                continue;
            }
            self.open(kind);
            names(self, &dependency.master, "");
            names(self, &dependency.dependent, "dependent_");
            self.opt("dependency_period", &dependency.period);
            self.flag("inherits_parent", dependency.inherits_parent);
            self.line(key, options.dump());
            self.close();
        }
    }

    fn escalation<Id>(
        &mut self,
        kind: &str,
        escalation: &Escalation<Id>,
        names: impl Fn(&mut Out, &Id, &str),
    ) {
        // The reference dump writes -2 for a number that is not given.
        let number = |value: Option<u32>| value.map_or(-2, i64::from);
        self.open(kind);
        names(self, &escalation.object, "");
        self.line("first_notification", number(escalation.first_notification));
        self.line("last_notification", number(escalation.last_notification));
        self.float("notification_interval", escalation.notification_interval);
        self.opt("escalation_period", &escalation.period);
        self.line("escalation_options", escalation.options.dump());
        self.list("contacts", &escalation.contacts);
        self.list("contact_groups", &escalation.contact_groups);
        self.close();
    }
}

fn host_names(out: &mut Out, host: &String, prefix: &str) {
    out.line(&format!("{prefix}host_name"), host);
}

fn service_names(out: &mut Out, (host, description): &ServiceId, prefix: &str) {
    out.line(&format!("{prefix}host_name"), host);
    out.line(&format!("{prefix}service_description"), description);
}

/// Writes every object, by type and then by name.
pub fn dump(objects: &Objects) -> String {
    let mut out = Out {
        text: String::new(),
    };

    for period in objects.timeperiods.values() {
        out.open("timeperiod");
        out.line("timeperiod_name", &period.name);
        out.line("alias", &period.alias);
        out.list("exclude", &period.exclude);
        let mut exceptions: Vec<_> = period.exceptions.iter().collect();
        exceptions.sort_by_key(|exception| exception.start.rank());
        let ranges = |ranges: &[crate::timeperiod::TimeRange]| {
            ranges
                .iter()
                .map(|range| range.to_string())
                .collect::<Vec<_>>()
                .join(",")
        };
        for exception in exceptions {
            out.line(&exception.days(), ranges(&exception.ranges));
        }
        for (day, times) in period
            .weekdays
            .iter()
            .enumerate()
            .filter(|(_, times)| !times.is_empty())
        {
            out.line(WEEKDAYS[day], ranges(times));
        }
        out.close();
    }

    for command in objects.commands.values() {
        out.open("command");
        out.line("command_name", &command.name);
        out.line("command_line", &command.line);
        out.close();
    }

    for group in objects.contactgroups.values() {
        out.group(
            "contactgroup",
            group,
            group.members.iter().cloned().collect(),
        );
    }
    for group in objects.hostgroups.values() {
        out.group("hostgroup", group, group.members.iter().cloned().collect());
    }
    for group in objects.servicegroups.values() {
        let members = group
            .members
            .iter()
            .flat_map(|(host, description)| [host.clone(), description.clone()]);
        out.group("servicegroup", group, members.collect());
    }

    for contact in objects.contacts.values() {
        out.open("contact");
        out.line("contact_name", &contact.name);
        out.line("alias", &contact.alias);
        out.opt(
            "service_notification_period",
            &contact.service_notification_period,
        );
        out.opt(
            "host_notification_period",
            &contact.host_notification_period,
        );
        out.line(
            "service_notification_options",
            contact.service_notification_options.dump(),
        );
        out.line(
            "host_notification_options",
            contact.host_notification_options.dump(),
        );
        out.list(
            "service_notification_commands",
            &contact.service_notification_commands,
        );
        out.list(
            "host_notification_commands",
            &contact.host_notification_commands,
        );
        out.opt("email", &contact.email);
        out.opt("pager", &contact.pager);
        for (n, address) in contact.addresses.iter().enumerate() {
            out.opt(&format!("address{}", n + 1), address);
        }
        out.line("minimum_value", contact.minimum_value);
        out.flag(
            "host_notifications_enabled",
            contact.host_notifications_enabled,
        );
        out.flag(
            "service_notifications_enabled",
            contact.service_notifications_enabled,
        );
        out.flag("can_submit_commands", contact.can_submit_commands);
        out.flag(
            "retain_status_information",
            contact.retain_status_information,
        );
        out.flag(
            "retain_nonstatus_information",
            contact.retain_nonstatus_information,
        );
        out.custom(&contact.custom);
        out.close();
    }

    for host in objects.hosts.values() {
        out.open("host");
        out.line("host_name", &host.name);
        out.opt("display_name", &host.check.display_name);
        out.line("alias", &host.alias);
        out.line("address", &host.address);
        out.list("parents", &host.parents);
        out.checkable(&host.check, None, Some(host));
        out.close();
    }

    // The reference sorts services on host name and description joined
    // with a semicolon, which is not the order of the pairs.
    let mut services: Vec<&Service> = objects.services.values().collect();
    services.sort_by_cached_key(|service| format!("{};{}", service.host, service.description));
    for service in services {
        out.open("service");
        out.line("host_name", &service.host);
        out.line("service_description", &service.description);
        out.opt("display_name", &service.check.display_name);
        // A single parent on the same host is written by description alone.
        let parents: Vec<String> = match service.parents.as_slice() {
            [(host, description)] if *host == service.host => vec![description.clone()],
            parents => parents
                .iter()
                .flat_map(|(host, description)| [host.clone(), description.clone()])
                .collect(),
        };
        out.list("parents", &parents);
        out.checkable(&service.check, Some(service.is_volatile), None);
        out.close();
    }

    for dependency in &objects.service_dependencies {
        out.dependency("servicedependency", dependency, service_names);
    }
    for escalation in &objects.service_escalations {
        out.escalation("serviceescalation", escalation, service_names);
    }
    for dependency in &objects.host_dependencies {
        out.dependency("hostdependency", dependency, host_names);
    }
    for escalation in &objects.host_escalations {
        out.escalation("hostescalation", escalation, host_names);
    }
    out.text
}
