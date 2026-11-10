//! Turns definitions into objects: applies templates, expands groups and
//! lists, fills in what objects take from each other, and checks the
//! result.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use regex::Regex;

use crate::diag::{Diagnostics, Loc};
use crate::model::*;
use crate::raw::{ObjType, RawObject, TIMEPERIOD_FIXED};
use crate::resolve::{Resolved, resolve};
use crate::schema;
use crate::timeperiod::{self, Entry};
use crate::values::{self, Opts};

/// Main configuration settings that change how object files are read.
#[derive(Clone, Debug)]
pub struct Settings {
    /// `use_regexp_matching`
    pub regexp: bool,
    /// `use_true_regexp_matching`
    pub true_regexp: bool,
    /// `illegal_object_name_chars`
    pub illegal_name_chars: String,
    /// `host_check_timeout`: the `check_timeout` of a host that sets none.
    pub host_check_timeout: u32,
    /// `service_check_timeout`, the same for a service.
    pub service_check_timeout: u32,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            regexp: false,
            true_regexp: false,
            illegal_name_chars: String::new(),
            host_check_timeout: 30,
            service_check_timeout: 60,
        }
    }
}

impl Settings {
    fn is_pattern(&self, item: &str) -> bool {
        self.true_regexp
            || (self.regexp && (item.contains(['*', '?', '+']) || item.contains("\\.")))
    }

    /// The names that one item of a list stands for: all of them for `*`,
    /// those a pattern matches, or the one with that name.
    fn select<'u>(&self, item: &str, universe: &[&'u str]) -> Result<Vec<&'u str>, String> {
        if item == "*" {
            return Ok(universe.to_vec());
        }
        if self.is_pattern(item) {
            let pattern = Regex::new(item)
                .map_err(|_| format!("'{item}' is not a valid regular expression"))?;
            return Ok(universe
                .iter()
                .copied()
                .filter(|name| pattern.is_match(name))
                .collect());
        }
        Ok(universe
            .iter()
            .copied()
            .filter(|name| *name == item)
            .collect())
    }
}

/// Names picked by a list and names taken out of it with `!`.
#[derive(Default)]
struct Picked {
    include: Vec<String>,
    exclude: BTreeSet<String>,
}

fn push_unique<T: PartialEq>(list: &mut Vec<T>, item: T) {
    if !list.contains(&item) {
        list.push(item);
    }
}

/// A definition that produced a service on one host.
struct Candidate {
    bag: Resolved,
    /// The host is named in `host_name`, not only reached through a group.
    by_host_name: bool,
}

struct Builder<'a> {
    settings: &'a Settings,
    diags: &'a mut Diagnostics,
    out: Objects,
    /// Hosts and services as text, after templates and after services took
    /// what they take from their host. Escalations take values from here.
    host_bags: BTreeMap<String, Resolved>,
    service_bags: BTreeMap<ServiceId, Resolved>,
    /// Where each object is defined, for messages.
    locs: HashMap<(ObjType, String), Loc>,
    service_parents: Vec<(ServiceId, Vec<String>)>,
}

/// Builds the objects of a configuration from every definition read.
pub fn build(mut raw: Vec<RawObject>, settings: &Settings, diags: &mut Diagnostics) -> Objects {
    // Settings are checked on every definition, templates included.
    for object in &mut raw {
        let mut seen = BTreeSet::new();
        for attr in &mut object.attrs {
            if attr.key.starts_with('_') {
                continue;
            }
            let loc = Loc {
                file: object.loc.file.clone(),
                line: attr.line,
            };
            if object.kind == ObjType::Timeperiod && !TIMEPERIOD_FIXED.contains(&attr.key.as_str())
            {
                match timeperiod::parse_entry(&attr.key, &attr.value) {
                    Err(why) => diags.error(Some(&loc), why),
                    Ok(_) => {
                        if let Some(warning) = timeperiod::times_warning(&attr.value) {
                            diags.warn(Some(&loc), format!("{}: {warning}", attr.key));
                        }
                    }
                }
                if !seen.insert(attr.key.clone()) {
                    diags.warn(
                        Some(&loc),
                        format!(
                            "'{}' is given twice in this definition; the later line is used",
                            attr.key
                        ),
                    );
                }
                continue;
            }
            let key = schema::canonical_key(object.kind, &attr.key).to_string();
            if !seen.insert(key.clone()) {
                diags.warn(
                    Some(&loc),
                    format!("'{key}' is given twice in this definition; the later line is used"),
                );
            }
            if attr.value.is_empty() {
                diags.warn(
                    Some(&loc),
                    format!("'{key}' has no value; it is kept as an empty value"),
                );
            }
            match schema::kind_of(object.kind, &key) {
                None => diags.error(
                    Some(&loc),
                    format!(
                        "'{}' is not a setting of a {}",
                        attr.key,
                        object.kind.keyword()
                    ),
                ),
                Some(kind) => match values::check(kind, &attr.value) {
                    Ok(None) => {}
                    Ok(Some(warning)) => {
                        diags.warn(Some(&loc), format!("{}: {warning}", attr.key));
                    }
                    Err(why) => diags.error(Some(&loc), format!("{}: {why}", attr.key)),
                },
            }
            attr.key = key;
        }
    }

    let mut resolved: HashMap<ObjType, Vec<Resolved>> = HashMap::new();
    for kind in ObjType::ALL {
        let of_kind: Vec<RawObject> = raw
            .iter()
            .filter(|object| object.kind == kind)
            .cloned()
            .collect();
        resolved.insert(kind, resolve(kind, &of_kind, diags));
    }
    let mut take = |kind| resolved.remove(&kind).unwrap_or_default();

    let mut builder = Builder {
        settings,
        diags,
        out: Objects::default(),
        host_bags: BTreeMap::new(),
        service_bags: BTreeMap::new(),
        locs: HashMap::new(),
        service_parents: Vec::new(),
    };
    builder.timeperiods(take(ObjType::Timeperiod));
    builder.commands(take(ObjType::Command));
    let contact_groups = builder.contacts(take(ObjType::Contact));
    builder.contactgroups(take(ObjType::ContactGroup), contact_groups);
    let host_groups = builder.hosts(take(ObjType::Host));
    builder.hostgroups(take(ObjType::HostGroup), host_groups);
    let service_groups = builder.services(take(ObjType::Service));
    builder.servicegroups(take(ObjType::ServiceGroup), service_groups);
    builder.host_dependencies(take(ObjType::HostDependency));
    builder.service_dependencies(take(ObjType::ServiceDependency));
    builder.host_escalations(take(ObjType::HostEscalation));
    builder.service_escalations(take(ObjType::ServiceEscalation));
    builder.check_references();
    builder.check_loops();
    builder.out
}

/// Typed access to resolved settings. The values were checked when the
/// files were read, so a value that does not parse here is one the earlier
/// check already reported.
impl Resolved {
    fn name(&self, key: &str) -> Option<String> {
        self.text(key)
            .filter(|value| !value.is_empty())
            .map(String::from)
    }

    fn owned(&self, key: &str) -> Option<String> {
        self.text(key).map(String::from)
    }

    fn flag(&self, key: &str, default: bool) -> bool {
        self.text(key)
            .and_then(|value| values::parse_flag(value).ok())
            .unwrap_or(default)
    }

    fn int(&self, key: &str, default: u32) -> u32 {
        self.text(key)
            .and_then(|value| values::parse_int(value).ok())
            .unwrap_or(default)
    }

    fn float(&self, key: &str, default: f64) -> f64 {
        self.text(key)
            .and_then(|value| values::parse_float(value).ok())
            .unwrap_or(default)
    }

    fn opts(&self, key: &str, allowed: &'static str, default: Opts) -> Opts {
        self.text(key)
            .and_then(|value| Opts::parse(value, allowed).ok())
            .unwrap_or(default)
    }
}

impl Builder<'_> {
    fn error(&mut self, loc: &Loc, message: impl Into<String>) {
        self.diags.error(Some(loc), message);
    }

    fn warn(&mut self, loc: &Loc, message: impl Into<String>) {
        self.diags.warn(Some(loc), message);
    }

    /// Checks the name of a new object and records where it is defined.
    /// Returns false when the object cannot be added.
    fn claim(&mut self, kind: ObjType, name: &str, loc: &Loc) -> bool {
        if let Some(bad) = name
            .chars()
            .find(|c| self.settings.illegal_name_chars.contains(*c))
        {
            self.error(
                loc,
                format!(
                    "{} name '{name}' contains '{bad}', which illegal_object_name_chars forbids",
                    kind.keyword()
                ),
            );
            return false;
        }
        if let Some(first) = self.locs.get(&(kind, name.to_string())) {
            let message = format!("{} '{name}' is already defined at {first}", kind.keyword());
            self.error(loc, message);
            return false;
        }
        self.locs.insert((kind, name.to_string()), loc.clone());
        true
    }

    fn loc_of(&self, kind: ObjType, name: &str) -> Option<Loc> {
        self.locs.get(&(kind, name.to_string())).cloned()
    }

    /// Expands a list of names, patterns, `*` and `!exclusions`.
    fn pick(&mut self, items: &[String], universe: &[&str], what: &str, loc: &Loc) -> Picked {
        let mut picked = Picked::default();
        for item in items {
            let (excluded, item) = match item.strip_prefix('!') {
                Some(rest) => (true, rest.trim()),
                None => (false, item.as_str()),
            };
            match self.settings.select(item, universe) {
                Err(why) => self.error(loc, why),
                Ok(names) if names.is_empty() => {
                    self.error(loc, format!("{what} '{item}' is not defined"))
                }
                Ok(names) => {
                    for name in names {
                        if excluded {
                            picked.exclude.insert(name.to_string());
                        } else {
                            push_unique(&mut picked.include, name.to_string());
                        }
                    }
                }
            }
        }
        picked
    }

    // ---- timeperiods and commands ----

    fn timeperiods(&mut self, resolved: Vec<Resolved>) {
        for bag in resolved {
            let (Some(name), Some(alias)) = (bag.name("timeperiod_name"), bag.owned("alias"))
            else {
                self.error(
                    &bag.loc,
                    "a timeperiod needs a timeperiod_name and an alias",
                );
                continue;
            };
            if !self.claim(ObjType::Timeperiod, &name, &bag.loc) {
                continue;
            }
            let mut period = Timeperiod {
                name: name.clone(),
                alias,
                exclude: bag.list("exclude"),
                weekdays: Default::default(),
                exceptions: Vec::new(),
            };
            for (key, value) in &bag.attrs {
                if TIMEPERIOD_FIXED.contains(&key.as_str()) || key.starts_with('_') {
                    continue;
                }
                match timeperiod::parse_entry(key, value) {
                    Ok(Entry::Weekday(day, ranges)) => period.weekdays[day] = ranges,
                    Ok(Entry::Exception(exception)) => period.exceptions.push(exception),
                    Err(_) => {}
                }
            }
            self.out.timeperiods.insert(name, period);
        }
    }

    fn commands(&mut self, resolved: Vec<Resolved>) {
        for bag in resolved {
            let (Some(name), Some(line)) = (bag.name("command_name"), bag.name("command_line"))
            else {
                self.error(
                    &bag.loc,
                    "a command needs a command_name and a command_line",
                );
                continue;
            };
            if self.claim(ObjType::Command, &name, &bag.loc) {
                self.out
                    .commands
                    .insert(name.clone(), Command { name, line });
            }
        }
    }

    // ---- contacts ----

    /// Returns the groups each contact asks to be in.
    fn contacts(&mut self, resolved: Vec<Resolved>) -> Vec<(String, String, Loc)> {
        let mut memberships = Vec::new();
        for bag in resolved {
            let Some(name) = bag.name("contact_name") else {
                self.error(&bag.loc, "a contact needs a contact_name");
                continue;
            };
            if !self.claim(ObjType::Contact, &name, &bag.loc) {
                continue;
            }
            for group in bag.list("contactgroups") {
                memberships.push((group, name.clone(), bag.loc.clone()));
            }
            let contact = Contact {
                name: name.clone(),
                alias: bag.owned("alias").unwrap_or_else(|| name.clone()),
                service_notification_period: bag.name("service_notification_period"),
                host_notification_period: bag.name("host_notification_period"),
                service_notification_options: bag.opts(
                    "service_notification_options",
                    schema::SERVICE_NOTIFY,
                    Opts::none(schema::SERVICE_NOTIFY),
                ),
                host_notification_options: bag.opts(
                    "host_notification_options",
                    schema::HOST_NOTIFY,
                    Opts::none(schema::HOST_NOTIFY),
                ),
                service_notification_commands: bag.list("service_notification_commands"),
                host_notification_commands: bag.list("host_notification_commands"),
                email: bag.owned("email"),
                pager: bag.owned("pager"),
                addresses: std::array::from_fn(|n| bag.owned(&format!("address{}", n + 1))),
                minimum_value: bag.int("minimum_value", 1),
                host_notifications_enabled: bag.flag("host_notifications_enabled", true),
                service_notifications_enabled: bag.flag("service_notifications_enabled", true),
                can_submit_commands: bag.flag("can_submit_commands", true),
                retain_status_information: bag.flag("retain_status_information", true),
                retain_nonstatus_information: bag.flag("retain_nonstatus_information", true),
                custom: bag.custom(),
            };
            if contact.service_notification_commands.is_empty() {
                self.error(
                    &bag.loc,
                    format!("contact '{name}' has no service_notification_commands"),
                );
            }
            if contact.host_notification_commands.is_empty() {
                self.error(
                    &bag.loc,
                    format!("contact '{name}' has no host_notification_commands"),
                );
            }
            if contact.service_notification_period.is_none() {
                self.warn(
                    &bag.loc,
                    format!("contact '{name}' has no service_notification_period"),
                );
            }
            if contact.host_notification_period.is_none() {
                self.warn(
                    &bag.loc,
                    format!("contact '{name}' has no host_notification_period"),
                );
            }
            self.out.contacts.insert(name, contact);
        }
        memberships
    }

    // ---- groups ----

    /// Reads the settings every group type has. Returns the group and the
    /// names of its member groups.
    fn group_shell<M: Ord>(
        &mut self,
        kind: ObjType,
        bag: &Resolved,
    ) -> Option<(Group<M>, Vec<String>)> {
        let word = kind.keyword();
        let Some(name) = bag.name(&format!("{word}_name")) else {
            self.error(&bag.loc, format!("a {word} needs a {word}_name"));
            return None;
        };
        if !self.claim(kind, &name, &bag.loc) {
            return None;
        }
        let group = Group {
            alias: bag.owned("alias").unwrap_or_else(|| name.clone()),
            name,
            members: BTreeSet::new(),
            notes: bag.owned("notes"),
            notes_url: bag.owned("notes_url"),
            action_url: bag.owned("action_url"),
        };
        Some((group, bag.list(&format!("{word}_members"))))
    }

    /// Adds the members that objects ask for themselves, then the members
    /// of member groups.
    fn finish_groups<M: Ord + Clone>(
        &mut self,
        kind: ObjType,
        groups: &mut BTreeMap<String, Group<M>>,
        nested: BTreeMap<String, Vec<String>>,
        asked: Vec<(String, M, Loc)>,
    ) {
        let word = kind.keyword();
        for (group, member, loc) in asked {
            match groups.get_mut(&group) {
                Some(group) => {
                    group.members.insert(member);
                }
                None => self.error(&loc, format!("{word} '{group}' is not defined")),
            }
        }
        let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (name, subgroups) in nested {
            let loc = self.loc_of(kind, &name);
            for subgroup in subgroups {
                if groups.contains_key(&subgroup) {
                    edges.entry(name.clone()).or_default().push(subgroup);
                } else {
                    self.diags.error(
                        loc.as_ref(),
                        format!("member group '{subgroup}' of {word} '{name}' is not defined"),
                    );
                }
            }
        }
        for name in groups.keys().cloned().collect::<Vec<_>>() {
            let mut reached = BTreeSet::new();
            let mut todo = edges.get(&name).cloned().unwrap_or_default();
            while let Some(next) = todo.pop() {
                if next == name {
                    let loc = self.loc_of(kind, &name);
                    self.diags.error(
                        loc.as_ref(),
                        format!("{word} '{name}' is a member of itself through its member groups"),
                    );
                } else if reached.insert(next.clone()) {
                    todo.extend(edges.get(&next).cloned().unwrap_or_default());
                }
            }
            // Only direct members are copied, and every group in reach is
            // visited, so the order of the copying does not matter.
            let direct: Vec<M> = reached
                .iter()
                .flat_map(|sub| direct_members(groups, &edges, sub))
                .collect();
            if let Some(group) = groups.get_mut(&name) {
                group.members.extend(direct);
            }
        }
    }

    fn contactgroups(&mut self, resolved: Vec<Resolved>, asked: Vec<(String, String, Loc)>) {
        let contacts: Vec<String> = self.out.contacts.keys().cloned().collect();
        let universe: Vec<&str> = contacts.iter().map(String::as_str).collect();
        let mut groups = BTreeMap::new();
        let mut nested = BTreeMap::new();
        for bag in resolved {
            let Some((mut group, subgroups)) =
                self.group_shell::<String>(ObjType::ContactGroup, &bag)
            else {
                continue;
            };
            let picked = self.pick(&bag.list("members"), &universe, "contact", &bag.loc);
            group.members = picked
                .include
                .into_iter()
                .filter(|name| !picked.exclude.contains(name))
                .collect();
            nested.insert(group.name.clone(), subgroups);
            groups.insert(group.name.clone(), group);
        }
        self.finish_groups(ObjType::ContactGroup, &mut groups, nested, asked);
        self.out.contactgroups = groups;
    }

    fn hostgroups(&mut self, resolved: Vec<Resolved>, asked: Vec<(String, String, Loc)>) {
        let hosts: Vec<String> = self.out.hosts.keys().cloned().collect();
        let universe: Vec<&str> = hosts.iter().map(String::as_str).collect();
        let mut groups = BTreeMap::new();
        let mut nested = BTreeMap::new();
        for bag in resolved {
            let Some((mut group, subgroups)) = self.group_shell::<String>(ObjType::HostGroup, &bag)
            else {
                continue;
            };
            let picked = self.pick(&bag.list("members"), &universe, "host", &bag.loc);
            group.members = picked
                .include
                .into_iter()
                .filter(|name| !picked.exclude.contains(name))
                .collect();
            nested.insert(group.name.clone(), subgroups);
            groups.insert(group.name.clone(), group);
        }
        self.finish_groups(ObjType::HostGroup, &mut groups, nested, asked);
        self.out.hostgroups = groups;
    }

    fn servicegroups(&mut self, resolved: Vec<Resolved>, asked: Vec<(String, ServiceId, Loc)>) {
        let mut groups = BTreeMap::new();
        let mut nested = BTreeMap::new();
        for bag in resolved {
            let Some((mut group, subgroups)) =
                self.group_shell::<ServiceId>(ObjType::ServiceGroup, &bag)
            else {
                continue;
            };
            let members = bag.list("members");
            if members.len() % 2 != 0 {
                self.error(
                    &bag.loc,
                    format!("members of servicegroup '{}' must be pairs of host name and service description", group.name),
                );
            }
            for pair in members.as_chunks::<2>().0 {
                let found = self.servicegroup_member(&pair[0], &pair[1], &group.name, &bag.loc);
                group.members.extend(found);
            }
            nested.insert(group.name.clone(), subgroups);
            groups.insert(group.name.clone(), group);
        }
        self.finish_groups(ObjType::ServiceGroup, &mut groups, nested, asked);
        self.out.servicegroups = groups;
    }

    /// The services that one `host,description` pair of a service group's
    /// members stands for.
    fn servicegroup_member(
        &mut self,
        host: &str,
        description: &str,
        group: &str,
        loc: &Loc,
    ) -> Vec<ServiceId> {
        let missing = || {
            format!(
                "service '{description}' on host '{host}', a member of servicegroup '{group}', is not defined"
            )
        };
        if host == "*" {
            self.error(
                loc,
                format!(
                    "servicegroup '{group}': '*' cannot stand for the host in a member; only the description can be '*'"
                ),
            );
            return Vec::new();
        }
        if self.settings.is_pattern(host) {
            // Naemon 1.5.2 takes every service of the hosts that match and
            // does not look at the description. The same is done here.
            let Ok(pattern) = Regex::new(host) else {
                self.error(loc, format!("'{host}' is not a valid regular expression"));
                return Vec::new();
            };
            let found: Vec<ServiceId> = self
                .out
                .services
                .keys()
                .filter(|(h, _)| pattern.is_match(h))
                .cloned()
                .collect();
            if found.is_empty() {
                self.error(loc, missing());
            } else {
                self.warn(
                    loc,
                    format!(
                        "servicegroup '{group}': the host '{host}' is a pattern, so every service of the hosts it \
                         matches is a member and the description '{description}' is not used. \
                         Naemon does this, and checkd does the same"
                    ),
                );
            }
            return found;
        }
        let on_host: Vec<&str> = self
            .out
            .services
            .keys()
            .filter(|(h, _)| h == host)
            .map(|(_, description)| description.as_str())
            .collect();
        match self.settings.select(description, &on_host) {
            Err(why) => {
                self.error(loc, why);
                Vec::new()
            }
            Ok(found) if found.is_empty() => {
                self.error(loc, missing());
                Vec::new()
            }
            Ok(found) => found
                .into_iter()
                .map(|description| (host.to_string(), description.to_string()))
                .collect(),
        }
    }

    // ---- hosts and services ----

    fn checkable(&mut self, bag: &Resolved, is_host: bool, what: &str) -> Checkable {
        let (states, notify, timeout) = if is_host {
            (
                schema::HOST_STATES,
                schema::HOST_NOTIFY,
                self.settings.host_check_timeout,
            )
        } else {
            (
                schema::SERVICE_STATES,
                schema::SERVICE_NOTIFY,
                self.settings.service_check_timeout,
            )
        };
        let initial_state = match bag.text("initial_state") {
            None => 'o',
            Some(value) if value.len() == 1 && states.contains(value) => {
                value.chars().next().unwrap_or('o')
            }
            Some(value) => {
                self.error(
                    &bag.loc,
                    format!("initial_state of {what} must be one state, not '{value}'"),
                );
                'o'
            }
        };
        match bag.text("max_check_attempts").map(values::parse_int) {
            Some(Ok(attempts)) if attempts >= 1 => {}
            // A value that is not a number was reported when it was read.
            Some(Err(_)) => {}
            _ => self.error(
                &bag.loc,
                format!("{what} needs max_check_attempts of 1 or more"),
            ),
        }
        Checkable {
            display_name: bag.owned("display_name"),
            check_period: bag.name("check_period"),
            check_command: bag.name("check_command"),
            event_handler: bag.name("event_handler"),
            contacts: bag.list("contacts"),
            contact_groups: bag.list("contact_groups"),
            notification_period: bag.name("notification_period"),
            initial_state,
            check_timeout: bag.int("check_timeout", timeout),
            hourly_value: bag.int("hourly_value", 1),
            check_interval: bag.float("check_interval", 5.0),
            retry_interval: bag.float("retry_interval", 1.0),
            max_check_attempts: bag.int("max_check_attempts", 1),
            active_checks_enabled: bag.flag("active_checks_enabled", true),
            passive_checks_enabled: bag.flag("passive_checks_enabled", true),
            obsess: bag.flag("obsess", true),
            event_handler_enabled: bag.flag("event_handler_enabled", true),
            low_flap_threshold: bag.float("low_flap_threshold", 0.0),
            high_flap_threshold: bag.float("high_flap_threshold", 0.0),
            flap_detection_enabled: bag.flag("flap_detection_enabled", true),
            flap_detection_options: bag.opts("flap_detection_options", states, Opts::all(states)),
            freshness_threshold: bag.int("freshness_threshold", 0),
            check_freshness: bag.flag("check_freshness", false),
            notification_options: bag.opts("notification_options", notify, Opts::all(notify)),
            notifications_enabled: bag.flag("notifications_enabled", true),
            notification_interval: bag.float("notification_interval", 30.0),
            first_notification_delay: bag.float("first_notification_delay", 0.0),
            stalking_options: bag.opts("stalking_options", states, Opts::none(states)),
            process_perf_data: bag.flag("process_perf_data", true),
            icon_image: bag.owned("icon_image"),
            icon_image_alt: bag.owned("icon_image_alt"),
            notes: bag.owned("notes"),
            notes_url: bag.owned("notes_url"),
            action_url: bag.owned("action_url"),
            retain_status_information: bag.flag("retain_status_information", true),
            retain_nonstatus_information: bag.flag("retain_nonstatus_information", true),
            custom: bag.custom(),
        }
    }

    /// Returns the groups each host asks to be in.
    fn hosts(&mut self, resolved: Vec<Resolved>) -> Vec<(String, String, Loc)> {
        let mut memberships = Vec::new();
        for bag in resolved {
            let Some(name) = bag.name("host_name") else {
                self.error(&bag.loc, "a host needs a host_name");
                continue;
            };
            if !self.claim(ObjType::Host, &name, &bag.loc) {
                continue;
            }
            for group in bag.list("hostgroups") {
                memberships.push((group, name.clone(), bag.loc.clone()));
            }
            let what = format!("host '{name}'");
            let host = Host {
                name: name.clone(),
                alias: bag.owned("alias").unwrap_or_else(|| name.clone()),
                address: bag.name("address").unwrap_or_else(|| name.clone()),
                parents: bag.list("parents"),
                check: self.checkable(&bag, true, &what),
                vrml_image: bag.owned("vrml_image"),
                statusmap_image: bag.owned("statusmap_image"),
                coords_2d: bag
                    .text("2d_coords")
                    .and_then(|value| values::parse_coords2(value).ok()),
                coords_3d: bag
                    .text("3d_coords")
                    .and_then(|value| values::parse_coords3(value).ok()),
            };
            if host.check.contacts.is_empty() && host.check.contact_groups.is_empty() {
                self.warn(
                    &bag.loc,
                    format!("{what} has no contacts and no contact_groups"),
                );
            }
            if host.check.check_command.is_none() {
                self.warn(
                    &bag.loc,
                    format!("{what} has no check_command, so it is never checked"),
                );
            }
            self.out.hosts.insert(name.clone(), host);
            self.host_bags.insert(name, bag);
        }
        memberships
    }

    /// The hosts a definition applies to, from a host list and a host
    /// group list. `None` when neither setting is given. The second set
    /// holds the hosts that the host list names.
    ///
    /// `lone_exclusions_ignored` is for host dependencies and host
    /// escalations. Naemon 1.5.2 ignores a `!host` in their host list
    /// when no host group is given with it. The same is done here, so
    /// that the same objects come out, with a warning.
    fn host_set(
        &mut self,
        bag: &Resolved,
        hosts_key: &str,
        groups_key: &str,
        lone_exclusions_ignored: bool,
    ) -> Option<(Vec<String>, BTreeSet<String>)> {
        if !bag.is_set(hosts_key) && !bag.is_set(groups_key) {
            return None;
        }
        let hosts: Vec<String> = self.out.hosts.keys().cloned().collect();
        let groups: Vec<String> = self.out.hostgroups.keys().cloned().collect();
        let mut host_items = bag.list(hosts_key);
        if lone_exclusions_ignored
            && !bag.is_set(groups_key)
            && host_items.iter().any(|item| item.starts_with('!'))
        {
            host_items.retain(|item| !item.starts_with('!'));
            self.warn(
                &bag.loc,
                format!(
                    "the '!' entries of {hosts_key} have no effect: Naemon ignores them in a host dependency or \
                     host escalation unless {groups_key} is given too, and checkd does the same"
                ),
            );
        }
        let by_name = self.pick(
            &host_items,
            &hosts.iter().map(String::as_str).collect::<Vec<_>>(),
            "host",
            &bag.loc,
        );
        let by_group = self.pick(
            &bag.list(groups_key),
            &groups.iter().map(String::as_str).collect::<Vec<_>>(),
            "hostgroup",
            &bag.loc,
        );
        let members = |names: &mut dyn Iterator<Item = &String>| -> Vec<String> {
            names
                .flat_map(|group| self.out.hostgroups[group].members.iter().cloned())
                .collect()
        };
        let mut exclude = by_name.exclude.clone();
        exclude.extend(members(&mut by_group.exclude.iter()));
        let mut all = by_name.include.clone();
        for host in members(&mut by_group.include.iter()) {
            push_unique(&mut all, host);
        }
        all.retain(|host| !exclude.contains(host));
        Some((all, by_name.include.into_iter().collect()))
    }

    /// Returns the groups each service asks to be in.
    fn services(&mut self, resolved: Vec<Resolved>) -> Vec<(String, ServiceId, Loc)> {
        let mut chosen: BTreeMap<ServiceId, Candidate> = BTreeMap::new();
        for bag in resolved {
            let description = bag.name("service_description");
            let hosts = self.host_set(&bag, "host_name", "hostgroup_name", false);
            let (Some(description), Some((hosts, named))) = (description, hosts) else {
                self.error(
                    &bag.loc,
                    "a service needs a service_description and a host_name or hostgroup_name",
                );
                continue;
            };
            if let Some(bad) = description
                .chars()
                .find(|c| self.settings.illegal_name_chars.contains(*c))
            {
                self.error(
                    &bag.loc,
                    format!("service description '{description}' contains '{bad}', which illegal_object_name_chars forbids"),
                );
                continue;
            }
            if hosts.is_empty() {
                self.warn(
                    &bag.loc,
                    format!("service '{description}' applies to no host, so it is dropped"),
                );
            }
            for host in hosts {
                let id = (host.clone(), description.clone());
                let candidate = Candidate {
                    bag: bag.clone(),
                    by_host_name: named.contains(&host),
                };
                // A definition that names the host wins over one that
                // reaches it through a group. Otherwise the later one wins.
                match chosen.get(&id) {
                    Some(earlier) if earlier.by_host_name && !candidate.by_host_name => {}
                    Some(earlier) => {
                        if earlier.by_host_name == candidate.by_host_name {
                            self.warn(
                                &bag.loc,
                                format!(
                                    "service '{description}' on host '{host}' is also defined at {}; this later definition is used",
                                    earlier.bag.loc
                                ),
                            );
                        }
                        chosen.insert(id, candidate);
                    }
                    None => {
                        chosen.insert(id, candidate);
                    }
                }
            }
        }

        let mut memberships = Vec::new();
        for (id, candidate) in chosen {
            let mut bag = candidate.bag;
            let (host, description) = id.clone();
            // What a service takes from its host when neither it nor its
            // templates give a value.
            if let Some(host_bag) = self.host_bags.get(&host) {
                let mut keys = vec![
                    "check_period",
                    "notification_interval",
                    "notification_period",
                ];
                if !bag.is_set("contacts") && !bag.is_set("contact_groups") {
                    keys.extend(["contacts", "contact_groups"]);
                }
                for key in keys {
                    if let (false, Some(value)) = (bag.is_set(key), host_bag.raw(key)) {
                        bag.set(key, value.to_string());
                    }
                }
            }
            for group in bag.list("servicegroups") {
                memberships.push((group, id.clone(), bag.loc.clone()));
            }
            let what = format!("service '{description}' on host '{host}'");
            let service = Service {
                host,
                description,
                parents: Vec::new(),
                is_volatile: bag.flag("is_volatile", false),
                check: self.checkable(&bag, false, &what),
            };
            if service.check.check_command.is_none() {
                self.error(&bag.loc, format!("{what} has no check_command"));
            }
            if service.check.contacts.is_empty() && service.check.contact_groups.is_empty() {
                self.warn(
                    &bag.loc,
                    format!("{what} has no contacts and no contact_groups"),
                );
            }
            self.service_parents.push((id.clone(), bag.list("parents")));
            self.out.services.insert(id.clone(), service);
            self.service_bags.insert(id, bag);
        }
        memberships
    }

    /// The services a dependency or escalation names, from host lists,
    /// host group lists, a service group list and a description list.
    fn service_set(
        &mut self,
        bag: &Resolved,
        hosts_key: &str,
        groups_key: &str,
        servicegroups_key: &str,
        descriptions_key: &str,
    ) -> Vec<ServiceId> {
        let mut found = Vec::new();
        let servicegroups: Vec<String> = self.out.servicegroups.keys().cloned().collect();
        let picked = self.pick(
            &bag.list(servicegroups_key),
            &servicegroups.iter().map(String::as_str).collect::<Vec<_>>(),
            "servicegroup",
            &bag.loc,
        );
        for group in picked
            .include
            .iter()
            .filter(|group| !picked.exclude.contains(*group))
        {
            for member in &self.out.servicegroups[group].members {
                push_unique(&mut found, member.clone());
            }
        }
        if let Some((hosts, _)) = self.host_set(bag, hosts_key, groups_key, false) {
            for host in hosts {
                for description in self.descriptions_on(bag, &host, descriptions_key) {
                    push_unique(&mut found, (host.clone(), description));
                }
            }
        }
        found
    }

    /// Expands a description list against the services of one host.
    fn descriptions_on(
        &mut self,
        bag: &Resolved,
        host: &str,
        descriptions_key: &str,
    ) -> Vec<String> {
        let on_host: Vec<String> = self
            .out
            .services
            .keys()
            .filter(|(h, _)| h == host)
            .map(|(_, description)| description.clone())
            .collect();
        let universe: Vec<&str> = on_host.iter().map(String::as_str).collect();
        let what = format!("on host '{host}', service");
        let picked = self.pick(&bag.list(descriptions_key), &universe, &what, &bag.loc);
        picked
            .include
            .into_iter()
            .filter(|description| !picked.exclude.contains(description))
            .collect()
    }

    // ---- dependencies ----

    fn dependency<Id: Clone + PartialEq + std::fmt::Debug>(
        &mut self,
        bag: &Resolved,
        allowed: &'static str,
        pairs: Vec<(Id, Id)>,
    ) -> Vec<Dependency<Id>> {
        let execution_failure = bag.opts("execution_failure_options", allowed, Opts::none(allowed));
        let notification_failure =
            bag.opts("notification_failure_options", allowed, Opts::none(allowed));
        if execution_failure.is_none() && notification_failure.is_none() {
            self.warn(
                &bag.loc,
                "this dependency names no failure states, so it has no effect and is dropped",
            );
            return Vec::new();
        }
        pairs
            .into_iter()
            .map(|(master, dependent)| Dependency {
                master,
                dependent,
                period: bag.name("dependency_period"),
                inherits_parent: bag.flag("inherits_parent", false),
                execution_failure: execution_failure.clone(),
                notification_failure: notification_failure.clone(),
            })
            .collect()
    }

    fn host_dependencies(&mut self, resolved: Vec<Resolved>) {
        for bag in resolved {
            let masters = self.host_set(&bag, "host_name", "hostgroup_name", true);
            let dependents = self.host_set(
                &bag,
                "dependent_host_name",
                "dependent_hostgroup_name",
                true,
            );
            let (Some((masters, _)), Some((dependents, _))) = (masters, dependents) else {
                self.error(
                    &bag.loc,
                    "a hostdependency needs a host and a dependent host, each by name or by group",
                );
                continue;
            };
            let pairs = masters
                .iter()
                .flat_map(|m| dependents.iter().map(move |d| (m.clone(), d.clone())))
                .collect();
            let built = self.dependency(&bag, schema::HOST_FAILURE, pairs);
            self.out.host_dependencies.extend(built);
        }
    }

    fn service_dependencies(&mut self, resolved: Vec<Resolved>) {
        for bag in resolved {
            let masters = self.service_set(
                &bag,
                "host_name",
                "hostgroup_name",
                "servicegroup_name",
                "service_description",
            );
            let elsewhere = [
                "dependent_host_name",
                "dependent_hostgroup_name",
                "dependent_servicegroup_name",
            ]
            .iter()
            .any(|key| bag.is_set(key));
            let mut pairs = Vec::new();
            if elsewhere {
                let dependents = self.service_set(
                    &bag,
                    "dependent_host_name",
                    "dependent_hostgroup_name",
                    "dependent_servicegroup_name",
                    "dependent_service_description",
                );
                for master in &masters {
                    for dependent in &dependents {
                        pairs.push((master.clone(), dependent.clone()));
                    }
                }
            } else {
                // No dependent host is named: the dependent services are
                // on the same host as the service they depend on.
                for master in &masters {
                    for description in
                        self.descriptions_on(&bag, &master.0, "dependent_service_description")
                    {
                        pairs.push((master.clone(), (master.0.clone(), description)));
                    }
                }
            }
            if pairs.is_empty() && !self.diags.has_errors() {
                self.warn(
                    &bag.loc,
                    "this servicedependency matches no pair of services",
                );
            }
            let built = self.dependency(&bag, schema::SERVICE_FAILURE, pairs);
            self.out.service_dependencies.extend(built);
        }
    }

    // ---- escalations ----

    /// Builds one escalation. `target` holds the settings of the host or
    /// service it is for: an escalation takes its contacts, interval and
    /// period from there when it gives none.
    fn escalation<Id>(
        &mut self,
        bag: &Resolved,
        target: Option<&Resolved>,
        object: Id,
        allowed: &'static str,
    ) -> Escalation<Id> {
        let from_target = |key: &str| target.map(|target| target.list(key)).unwrap_or_default();
        let alone = !bag.is_set("contacts") && !bag.is_set("contact_groups");
        let merged = |key: &str| {
            let mut list = bag.list(key);
            if alone || bag.is_additive(key) {
                for item in from_target(key) {
                    push_unique(&mut list, item);
                }
            }
            list
        };
        let target_text = |key: &str| target.and_then(|target| target.text(key)).map(String::from);
        let interval = bag
            .text("notification_interval")
            .map(String::from)
            .or_else(|| target_text("notification_interval"));
        Escalation {
            object,
            first_notification: bag
                .text("first_notification")
                .and_then(|value| values::parse_int(value).ok()),
            last_notification: bag
                .text("last_notification")
                .and_then(|value| values::parse_int(value).ok()),
            notification_interval: interval
                .and_then(|value| values::parse_float(&value).ok())
                .unwrap_or(0.0),
            period: bag
                .name("escalation_period")
                .or_else(|| target_text("notification_period")),
            options: bag.opts("escalation_options", allowed, Opts::all(allowed)),
            contacts: merged("contacts"),
            contact_groups: merged("contact_groups"),
        }
    }

    /// Warns about a range of notifications that Naemon accepts without a
    /// word: none given at all, or one that ends before it starts.
    fn check_escalation_range(&mut self, bag: &Resolved) {
        let number = |key: &str| {
            bag.text(key)
                .and_then(|value| values::parse_int(value).ok())
        };
        match (number("first_notification"), number("last_notification")) {
            (None, None) => self.warn(
                &bag.loc,
                "this escalation gives no first_notification and no last_notification",
            ),
            (Some(first), Some(last)) if last != 0 && first > last => self.warn(
                &bag.loc,
                format!(
                    "this escalation starts at notification {first} and ends at {last}, so it covers no notification"
                ),
            ),
            _ => {}
        }
    }

    fn host_escalations(&mut self, resolved: Vec<Resolved>) {
        for bag in resolved {
            self.check_escalation_range(&bag);
            let Some((hosts, _)) = self.host_set(&bag, "host_name", "hostgroup_name", true) else {
                self.error(
                    &bag.loc,
                    "a hostescalation needs a host_name or hostgroup_name",
                );
                continue;
            };
            for host in hosts {
                let target = self.host_bags.get(&host).cloned();
                let built = self.escalation(&bag, target.as_ref(), host, schema::HOST_ESCALATION);
                self.out.host_escalations.push(built);
            }
        }
    }

    fn service_escalations(&mut self, resolved: Vec<Resolved>) {
        for bag in resolved {
            self.check_escalation_range(&bag);
            let services = self.service_set(
                &bag,
                "host_name",
                "hostgroup_name",
                "servicegroup_name",
                "service_description",
            );
            if services.is_empty() && !self.diags.has_errors() {
                self.warn(&bag.loc, "this serviceescalation matches no service");
            }
            for id in services {
                let target = self.service_bags.get(&id).cloned();
                let built = self.escalation(&bag, target.as_ref(), id, schema::SERVICE_ESCALATION);
                self.out.service_escalations.push(built);
            }
        }
    }

    // ---- checks across objects ----

    fn check_references(&mut self) {
        let mut errors: Vec<(Option<Loc>, String)> = Vec::new();
        let out = &self.out;
        let period = |name: &Option<String>| {
            name.as_ref()
                .filter(|name| !out.timeperiods.contains_key(*name))
                .cloned()
        };
        // A command is written name!arg1!arg2.
        let command = |line: &Option<String>| {
            line.as_ref()
                .filter(|line| {
                    !out.commands
                        .contains_key(line.split('!').next().unwrap_or(""))
                })
                .cloned()
        };
        let people = |check: &Checkable,
                      what: &str,
                      loc: &Option<Loc>,
                      errors: &mut Vec<(Option<Loc>, String)>| {
            for name in check
                .contacts
                .iter()
                .filter(|name| !out.contacts.contains_key(*name))
            {
                errors.push((
                    loc.clone(),
                    format!("contact '{name}' of {what} is not defined"),
                ));
            }
            for name in check
                .contact_groups
                .iter()
                .filter(|name| !out.contactgroups.contains_key(*name))
            {
                errors.push((
                    loc.clone(),
                    format!("contactgroup '{name}' of {what} is not defined"),
                ));
            }
        };
        let checkable = |check: &Checkable,
                         what: &str,
                         loc: &Option<Loc>,
                         errors: &mut Vec<(Option<Loc>, String)>| {
            for (setting, missing) in [
                ("check_period", period(&check.check_period)),
                ("notification_period", period(&check.notification_period)),
            ] {
                if let Some(name) = missing {
                    errors.push((
                        loc.clone(),
                        format!("timeperiod '{name}' ({setting} of {what}) is not defined"),
                    ));
                }
            }
            for (setting, missing) in [
                ("check_command", command(&check.check_command)),
                ("event_handler", command(&check.event_handler)),
            ] {
                if let Some(line) = missing {
                    errors.push((
                        loc.clone(),
                        format!("command '{line}' ({setting} of {what}) is not defined"),
                    ));
                }
            }
            people(check, what, loc, errors);
        };

        for (name, timeperiod) in &out.timeperiods {
            let loc = self.loc_of(ObjType::Timeperiod, name);
            for excluded in timeperiod
                .exclude
                .iter()
                .filter(|excluded| !out.timeperiods.contains_key(*excluded))
            {
                errors.push((
                    loc.clone(),
                    format!("timeperiod '{excluded}', excluded by '{name}', is not defined"),
                ));
            }
        }
        for (name, contact) in &out.contacts {
            let loc = self.loc_of(ObjType::Contact, name);
            for missing in [
                period(&contact.host_notification_period),
                period(&contact.service_notification_period),
            ]
            .into_iter()
            .flatten()
            {
                errors.push((
                    loc.clone(),
                    format!("timeperiod '{missing}' of contact '{name}' is not defined"),
                ));
            }
            for line in contact
                .host_notification_commands
                .iter()
                .chain(&contact.service_notification_commands)
            {
                if command(&Some(line.clone())).is_some() {
                    errors.push((
                        loc.clone(),
                        format!("notification command '{line}' of contact '{name}' is not defined"),
                    ));
                }
            }
        }
        for (name, host) in &out.hosts {
            let loc = self.loc_of(ObjType::Host, name);
            let what = format!("host '{name}'");
            checkable(&host.check, &what, &loc, &mut errors);
            for parent in &host.parents {
                if parent == name {
                    errors.push((loc.clone(), format!("{what} names itself as a parent")));
                } else if !out.hosts.contains_key(parent) {
                    errors.push((
                        loc.clone(),
                        format!("parent '{parent}' of {what} is not defined"),
                    ));
                }
            }
        }
        for (id, service) in &out.services {
            let loc = self.service_bags.get(id).map(|bag| bag.loc.clone());
            let what = format!("service '{}' on host '{}'", id.1, id.0);
            checkable(&service.check, &what, &loc, &mut errors);
        }
        for dependency in &out.host_dependencies {
            if let Some(name) = period(&dependency.period) {
                errors.push((
                    None,
                    format!("timeperiod '{name}' (dependency_period) is not defined"),
                ));
            }
        }
        for dependency in &out.service_dependencies {
            if let Some(name) = period(&dependency.period) {
                errors.push((
                    None,
                    format!("timeperiod '{name}' (dependency_period) is not defined"),
                ));
            }
        }
        let escalation_people =
            |contacts: &[String],
             groups: &[String],
             period_name: &Option<String>,
             what: &str,
             errors: &mut Vec<(Option<Loc>, String)>| {
                for name in contacts
                    .iter()
                    .filter(|name| !out.contacts.contains_key(*name))
                {
                    errors.push((None, format!("contact '{name}' of {what} is not defined")));
                }
                for name in groups
                    .iter()
                    .filter(|name| !out.contactgroups.contains_key(*name))
                {
                    errors.push((
                        None,
                        format!("contactgroup '{name}' of {what} is not defined"),
                    ));
                }
                if let Some(name) = period(period_name) {
                    errors.push((
                        None,
                        format!("timeperiod '{name}' (escalation_period of {what}) is not defined"),
                    ));
                }
            };
        for escalation in &out.host_escalations {
            let what = format!("an escalation for host '{}'", escalation.object);
            escalation_people(
                &escalation.contacts,
                &escalation.contact_groups,
                &escalation.period,
                &what,
                &mut errors,
            );
        }
        for escalation in &out.service_escalations {
            let what = format!(
                "an escalation for service '{}' on host '{}'",
                escalation.object.1, escalation.object.0
            );
            escalation_people(
                &escalation.contacts,
                &escalation.contact_groups,
                &escalation.period,
                &what,
                &mut errors,
            );
        }

        // Parent services: one description for a service on the same host,
        // or pairs of host name and description.
        let mut parents = Vec::new();
        for (id, items) in &self.service_parents {
            let loc = self.service_bags.get(id).map(|bag| bag.loc.clone());
            let what = format!("service '{}' on host '{}'", id.1, id.0);
            let wanted: Vec<ServiceId> = match items.as_slice() {
                [description] => vec![(id.0.clone(), description.clone())],
                _ if items.len() % 2 != 0 => {
                    errors.push((
                        loc.clone(),
                        format!("parents of {what} must be one description, or pairs of host name and description"),
                    ));
                    Vec::new()
                }
                _ => items
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| (pair[0].clone(), pair[1].clone()))
                    .collect(),
            };
            let mut found = Vec::new();
            for parent in wanted {
                if out.services.contains_key(&parent) {
                    found.push(parent);
                } else {
                    errors.push((
                        loc.clone(),
                        format!(
                            "parent service '{}' on host '{}' of {what} is not defined",
                            parent.1, parent.0
                        ),
                    ));
                }
            }
            parents.push((id.clone(), found));
        }
        for (loc, message) in errors {
            self.diags.error(loc.as_ref(), message);
        }
        for (id, found) in parents {
            if let Some(service) = self.out.services.get_mut(&id) {
                service.parents = found;
            }
        }
    }

    fn check_loops(&mut self) {
        let out = &self.out;
        let mut errors = Vec::new();

        let hosts: BTreeMap<&str, Vec<&str>> = out
            .hosts
            .iter()
            .map(|(name, host)| {
                (
                    name.as_str(),
                    host.parents.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        for name in on_a_loop(&hosts) {
            errors.push(format!(
                "host '{name}' is its own ancestor through 'parents'"
            ));
        }

        let periods: BTreeMap<&str, Vec<&str>> = out
            .timeperiods
            .iter()
            .map(|(name, period)| {
                (
                    name.as_str(),
                    period.exclude.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        for name in on_a_loop(&periods) {
            errors.push(format!(
                "timeperiod '{name}' excludes itself, directly or through other timeperiods"
            ));
        }

        // Execution and notification dependencies are separate chains.
        for execution in [true, false] {
            let kind = if execution {
                "execution"
            } else {
                "notification"
            };
            let used = |execution_failure: &Opts, notification_failure: &Opts| {
                !(if execution {
                    execution_failure
                } else {
                    notification_failure
                })
                .is_none()
            };
            let mut edges: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
            for dependency in &out.host_dependencies {
                if used(
                    &dependency.execution_failure,
                    &dependency.notification_failure,
                ) {
                    edges
                        .entry(&dependency.dependent)
                        .or_default()
                        .push(&dependency.master);
                }
            }
            for name in on_a_loop(&edges) {
                errors.push(format!(
                    "host '{name}' depends on itself through {kind} dependencies"
                ));
            }
            let mut edges: BTreeMap<&ServiceId, Vec<&ServiceId>> = BTreeMap::new();
            for dependency in &out.service_dependencies {
                if used(
                    &dependency.execution_failure,
                    &dependency.notification_failure,
                ) {
                    edges
                        .entry(&dependency.dependent)
                        .or_default()
                        .push(&dependency.master);
                }
            }
            for (host, description) in on_a_loop(&edges) {
                errors.push(format!("service '{description}' on host '{host}' depends on itself through {kind} dependencies"));
            }
        }
        for message in errors {
            self.diags.error(None, message);
        }
    }
}

/// The direct members of one group, as they are before member groups are
/// added. `finish_groups` fills groups in name order, so a group earlier
/// in that order may already hold more than its direct members. That adds
/// only members which belong in the result anyway.
fn direct_members<M: Ord + Clone>(
    groups: &BTreeMap<String, Group<M>>,
    _edges: &BTreeMap<String, Vec<String>>,
    name: &str,
) -> Vec<M> {
    groups
        .get(name)
        .map(|group| group.members.iter().cloned().collect())
        .unwrap_or_default()
}

/// The nodes of a graph that can reach themselves.
fn on_a_loop<N: Ord + Copy>(edges: &BTreeMap<N, Vec<N>>) -> Vec<N> {
    let mut looping = Vec::new();
    for &start in edges.keys() {
        let mut seen = BTreeSet::new();
        let mut todo: Vec<N> = edges[&start].clone();
        while let Some(next) = todo.pop() {
            if next == start {
                looping.push(start);
                break;
            }
            if seen.insert(next) {
                todo.extend(edges.get(&next).into_iter().flatten().copied());
            }
        }
    }
    looping
}
