//! Turns definitions into objects: applies templates, expands groups and
//! lists, fills in what objects take from each other, and checks the
//! result.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

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
    /// `allow_empty_hostgroup_assignment`. Its one effect on loading: when
    /// it is off, a host dependency with a side that comes to no host is an
    /// error. When it is on, such a dependency is dropped.
    pub allow_empty_hostgroups: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            regexp: false,
            true_regexp: false,
            illegal_name_chars: String::new(),
            allow_empty_hostgroups: true,
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

    /// Whether `*` alone stands for every name. With `use_regexp_matching`
    /// on, Naemon reads it as a regular expression in a list of hosts,
    /// contacts or groups, and as such it is not valid. In a list of
    /// service descriptions it keeps its meaning until
    /// `use_true_regexp_matching` is on as well.
    fn star_is_all(&self, descriptions: bool) -> bool {
        !self.true_regexp && (descriptions || !self.regexp)
    }

    /// The names that one item of a list stands for: all of them for `*`,
    /// those a pattern matches, or the one with that name.
    fn select<'u, S: AsRef<str>>(
        &self,
        item: &str,
        universe: &'u [S],
        descriptions: bool,
    ) -> Result<Vec<&'u str>, String> {
        if item == "*" {
            if self.star_is_all(descriptions) {
                return Ok(universe.iter().map(AsRef::as_ref).collect());
            }
            return Err(
                "'*' does not stand for every name here: with regexp matching on it is read as a regular \
                 expression, and it is not a valid one. '.*' matches every name"
                    .to_string(),
            );
        }
        if self.is_pattern(item) {
            let pattern = Regex::new(item)
                .map_err(|_| format!("'{item}' is not a valid regular expression"))?;
            return Ok(universe
                .iter()
                .map(AsRef::as_ref)
                .filter(|name| pattern.is_match(name))
                .collect());
        }
        // The universes are made from sorted maps.
        debug_assert!(universe.windows(2).all(|pair| pair[0].as_ref() <= pair[1].as_ref()));
        Ok(match universe.binary_search_by(|name| name.as_ref().cmp(item)) {
            Ok(at) => vec![universe[at].as_ref()],
            Err(_) => Vec::new(),
        })
    }
}

/// What objects ask for with their own `hostgroups`, `servicegroups` or
/// `contactgroups` setting: the list as written, the object, and where.
type Asked<M> = Vec<(Vec<String>, M, Loc)>;

/// Names picked by a list and names taken out of it with `!`.
#[derive(Default)]
struct Picked {
    /// The names picked, each one once.
    include: Vec<String>,
    /// The names picked, each one as often as the list brings it up.
    mentions: Vec<String>,
    exclude: BTreeSet<String>,
    /// An item of the list was reported as an error.
    faulty: bool,
}

impl Picked {
    /// The names picked and not taken out again.
    fn kept(&self) -> Vec<String> {
        self.include
            .iter()
            .filter(|name| !self.exclude.contains(*name))
            .cloned()
            .collect()
    }
}

/// Sets a value that is not set yet.
fn fill<T>(slot: &mut Option<T>, value: Option<T>) {
    if slot.is_none() {
        *slot = value;
    }
}

/// The extended-info settings that hosts and services share.
fn fill_extinfo(check: &mut Checkable, bag: &Resolved) {
    fill(&mut check.notes, bag.owned("notes"));
    fill(&mut check.notes_url, bag.owned("notes_url"));
    fill(&mut check.action_url, bag.owned("action_url"));
    fill(&mut check.icon_image, bag.owned("icon_image"));
    fill(&mut check.icon_image_alt, bag.owned("icon_image_alt"));
}

fn push_unique<T: PartialEq>(list: &mut Vec<T>, item: T) {
    if !list.contains(&item) {
        list.push(item);
    }
}

/// Joins the lines of a setting that is given more than once in one
/// definition, for the settings where the reference adds the lines
/// together instead of using the later one. A line that says `null` adds
/// nothing.
fn add_up_lines(object: &mut RawObject) {
    let kind = object.kind;
    let mut index = 0;
    while index < object.attrs.len() {
        let key = object.attrs[index].key.clone();
        if schema::lines_add_up(kind, &key) {
            let mut later = Vec::new();
            let mut at = index + 1;
            while at < object.attrs.len() {
                if object.attrs[at].key == key {
                    later.push(object.attrs.remove(at).value);
                } else {
                    at += 1;
                }
            }
            let first = &mut object.attrs[index].value;
            for value in later {
                if value == "null" || value.is_empty() {
                    continue;
                }
                if first == "null" || first.is_empty() {
                    *first = value;
                } else {
                    first.push(',');
                    first.push_str(&value);
                }
            }
        }
        index += 1;
    }
}

/// A definition that produced a service on one host.
/// See `Builder::side`.
#[derive(Clone, Copy, PartialEq)]
enum Side {
    /// None of the four settings.
    Nothing,
    /// A description and nothing to say on which hosts. In a service
    /// dependency the hosts are then those of the other side.
    DescriptionOnly,
    /// Hosts with a description, or a service group, or both.
    Named,
    /// A combination that Naemon refuses. The text says what is given.
    Faulty(&'static str),
}

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
    /// While set, errors are gathered here and not reported. Used for a
    /// host dependency, which Naemon drops where it refuses other objects.
    held_back: Option<Vec<String>>,
    /// The names of the hosts and of the host groups, sorted, for lists
    /// that name them: made again only when there are more.
    host_names: Vec<String>,
    hostgroup_names: Vec<String>,
}

/// Builds the objects of a configuration from every definition read.
pub fn build(mut raw: Vec<RawObject>, settings: &Settings, diags: &mut Diagnostics) -> Objects {
    // Settings are checked on every definition, templates included.
    for object in &mut raw {
        let mut seen = BTreeSet::new();
        if object.kind.is_extinfo() {
            diags.warn(
                Some(&object.loc),
                format!(
                    "{} is deprecated in Naemon; the same settings can be given on the {} itself",
                    object.kind.keyword(),
                    if object.kind == ObjType::HostExtInfo {
                        "host"
                    } else {
                        "service"
                    }
                ),
            );
        }
        for attr in &mut object.attrs {
            let loc = Loc {
                file: object.loc.file.clone(),
                line: attr.line,
            };
            if attr.key.starts_with('_') {
                if object.kind.is_extinfo() {
                    diags.error(
                        Some(&loc),
                        format!(
                            "'{}': a {} takes no custom variables",
                            attr.key,
                            object.kind.keyword()
                        ),
                    );
                }
                continue;
            }
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
                let outcome = if schema::lines_add_up(object.kind, &key) {
                    "the lines are added together"
                } else {
                    "the later line is used"
                };
                diags.warn(
                    Some(&loc),
                    format!("'{key}' is given twice in this definition; {outcome}"),
                );
            }
            if attr.value.is_empty() {
                diags.warn(
                    Some(&loc),
                    format!("'{key}' has no value; it is kept as an empty value"),
                );
            }
            match schema::kind_of(object.kind, &key) {
                Some(schema::Kind::NoEffect) => diags.warn(
                    Some(&loc),
                    format!("'{key}' is a setting of older Nagios versions and has no effect"),
                ),
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
        let kind = object.kind;
        object
            .attrs
            .retain(|attr| schema::kind_of(kind, &attr.key) != Some(schema::Kind::NoEffect));
        add_up_lines(object);
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
        held_back: None,
        host_names: Vec::new(),
        hostgroup_names: Vec::new(),
    };
    builder.timeperiods(take(ObjType::Timeperiod));
    builder.commands(take(ObjType::Command));
    let contact_groups = builder.contacts(take(ObjType::Contact));
    builder.contactgroups(take(ObjType::ContactGroup), contact_groups);
    let host_groups = builder.hosts(take(ObjType::Host));
    builder.hostgroups(take(ObjType::HostGroup), host_groups);
    let service_groups = builder.services(take(ObjType::Service));
    builder.servicegroups(take(ObjType::ServiceGroup), service_groups);
    builder.host_extinfo(take(ObjType::HostExtInfo));
    builder.service_extinfo(take(ObjType::ServiceExtInfo));
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

    /// A value where `null` is a word like any other: names, aliases,
    /// addresses and command lines. Naemon 1.5.2 makes a contact called
    /// "null" from `contact_name null`.
    fn literal(&self, key: &str) -> Option<String> {
        self.raw(key)
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
        match &mut self.held_back {
            Some(held) => held.push(message.into()),
            None => self.diags.error(Some(loc), message),
        }
    }

    /// Reports a definition that lacks what makes it an object. One with
    /// a template name is taken for a template, as Naemon does without a
    /// word. Any other is an error.
    fn nameless(&mut self, bag: &Resolved, what: &str, needs: &str) {
        match &bag.template_name {
            Some(template) => self.warn(
                &bag.loc,
                format!(
                    "{what} template '{template}' has no {needs} and no 'register 0'; it is used as a template only"
                ),
            ),
            None => self.error(&bag.loc, format!("a {what} needs a {needs}")),
        }
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
    fn pick<S: AsRef<str>>(&mut self, items: &[String], universe: &[S], what: &str, loc: &Loc) -> Picked {
        self.pick_from(items, universe, what, loc, false)
    }

    /// `descriptions` says that the list is one of service descriptions,
    /// where `*` is read differently (`Settings::star_is_all`).
    fn pick_from<S: AsRef<str>>(
        &mut self,
        items: &[String],
        universe: &[S],
        what: &str,
        loc: &Loc,
        descriptions: bool,
    ) -> Picked {
        let mut picked = Picked::default();
        for item in items {
            let (excluded, item) = match item.strip_prefix('!') {
                Some(rest) => (true, rest.trim()),
                None => (false, item.as_str()),
            };
            match self.settings.select(item, universe, descriptions) {
                Err(why) => {
                    picked.faulty = true;
                    self.error(loc, why)
                }
                Ok(names) if names.is_empty() => {
                    picked.faulty = true;
                    self.error(loc, format!("{what} '{item}' is not defined"))
                }
                Ok(names) => {
                    for name in names {
                        if excluded {
                            picked.exclude.insert(name.to_string());
                        } else {
                            push_unique(&mut picked.include, name.to_string());
                            picked.mentions.push(name.to_string());
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
            let Some(name) = bag.literal("timeperiod_name") else {
                self.nameless(&bag, "timeperiod", "timeperiod_name");
                continue;
            };
            let Some(alias) = bag.literal("alias") else {
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
            let Some(name) = bag.literal("command_name") else {
                self.nameless(&bag, "command", "command_name");
                continue;
            };
            let Some(line) = bag.literal("command_line") else {
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
    fn contacts(&mut self, resolved: Vec<Resolved>) -> Asked<String> {
        let mut memberships = Vec::new();
        for bag in resolved {
            let Some(name) = bag.literal("contact_name") else {
                self.nameless(&bag, "contact", "contact_name");
                continue;
            };
            if !self.claim(ObjType::Contact, &name, &bag.loc) {
                continue;
            }
            memberships.push((bag.list("contactgroups"), name.clone(), bag.loc.clone()));
            let contact = Contact {
                name: name.clone(),
                alias: bag.literal("alias").unwrap_or_else(|| name.clone()),
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
        let Some(name) = bag.literal(&format!("{word}_name")) else {
            self.nameless(bag, word, &format!("{word}_name"));
            return None;
        };
        if !self.claim(kind, &name, &bag.loc) {
            return None;
        }
        let group = Group {
            alias: bag.literal("alias").unwrap_or_else(|| name.clone()),
            name,
            members: BTreeSet::new(),
            notes: bag.owned("notes"),
            notes_url: bag.owned("notes_url"),
            action_url: bag.owned("action_url"),
        };
        Some((group, bag.list(&format!("{word}_members"))))
    }

    /// The groups that objects join with their own `hostgroups`,
    /// `servicegroups` or `contactgroups` setting, one entry for each
    /// group and member. The setting is a list like any other: it takes
    /// `*`, patterns and `!` exclusions. A list that is given and leaves
    /// no group is an error.
    fn joined<M: Clone>(
        &mut self,
        kind: ObjType,
        asked: Asked<M>,
        groups: &BTreeMap<String, Group<impl Ord>>,
    ) -> Vec<(String, M, Loc)> {
        let word = kind.keyword();
        let universe: Vec<&str> = groups.keys().map(String::as_str).collect();
        // A service definition for many hosts asks once for each host.
        // Its list is worked out, and its faults reported, once.
        let mut worked_out: BTreeMap<(Arc<PathBuf>, u32, Vec<String>), Vec<String>> =
            BTreeMap::new();
        let mut joined = Vec::new();
        for (items, member, loc) in asked {
            if items.is_empty() {
                continue;
            }
            let key = (loc.file.clone(), loc.line, items.clone());
            if !worked_out.contains_key(&key) {
                let picked = self.pick(&items, &universe, word, &loc);
                let kept = picked.kept();
                if kept.is_empty() && !picked.faulty {
                    self.error(
                        &loc,
                        format!("the {word}s setting '{}' leaves no {word}", items.join(",")),
                    );
                }
                worked_out.insert(key.clone(), kept);
            }
            for group in &worked_out[&key] {
                joined.push((group.clone(), member.clone(), loc.clone()));
            }
        }
        joined
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

    fn contactgroups(&mut self, resolved: Vec<Resolved>, asked: Asked<String>) {
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
        let asked = self.joined(ObjType::ContactGroup, asked, &groups);
        self.finish_groups(ObjType::ContactGroup, &mut groups, nested, asked);
        self.out.contactgroups = groups;
    }

    fn hostgroups(&mut self, resolved: Vec<Resolved>, asked: Asked<String>) {
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
        let asked = self.joined(ObjType::HostGroup, asked, &groups);
        self.finish_groups(ObjType::HostGroup, &mut groups, nested, asked);
        self.out.hostgroups = groups;
    }

    fn servicegroups(&mut self, resolved: Vec<Resolved>, asked: Asked<ServiceId>) {
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
        let asked = self.joined(ObjType::ServiceGroup, asked, &groups);
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
        if !host.starts_with('!') && description.starts_with('!') {
            self.warn(
                loc,
                format!(
                    "servicegroup '{group}': the member '{host},{description}' adds nothing and takes nothing out; \
                     Naemon accepts it without a word, and checkd does the same"
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
        match self.settings.select(description, &on_host, true) {
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
    fn hosts(&mut self, resolved: Vec<Resolved>) -> Asked<String> {
        let mut memberships = Vec::new();
        for bag in resolved {
            let Some(name) = bag.literal("host_name") else {
                self.nameless(&bag, "host", "host_name");
                continue;
            };
            if !self.claim(ObjType::Host, &name, &bag.loc) {
                continue;
            }
            memberships.push((bag.list("hostgroups"), name.clone(), bag.loc.clone()));
            let what = format!("host '{name}'");
            let host = Host {
                name: name.clone(),
                alias: bag.literal("alias").unwrap_or_else(|| name.clone()),
                address: bag.literal("address").unwrap_or_else(|| name.clone()),
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
    /// group list, each host once. `None` when neither setting is given.
    /// The second set holds the hosts that the host list names.
    ///
    /// `named_hosts_stay` is for host dependencies, host escalations and
    /// extended host info. See `host_mentions`.
    fn host_set(
        &mut self,
        bag: &Resolved,
        hosts_key: &str,
        groups_key: &str,
        named_hosts_stay: bool,
    ) -> Option<(Vec<String>, BTreeSet<String>)> {
        let (mentions, named) = self.host_mentions(bag, hosts_key, groups_key, named_hosts_stay)?;
        let mut all = Vec::new();
        for host in mentions {
            push_unique(&mut all, host);
        }
        Some((all, named))
    }

    /// The hosts a definition applies to, each one as often as the two
    /// lists bring it up: `web,web` gives the members of `web` twice, and
    /// a host that is named and is in a named group comes twice.
    ///
    /// With `named_hosts_stay`, a `!` takes hosts out of the host groups
    /// only. A host that `host_name` names stays, whatever is excluded by
    /// name or by group, so in a host list with no host group a `!host`
    /// does nothing at all. Naemon 1.5.2 does this for host dependencies,
    /// host escalations and extended host info. The same is done here, so
    /// that the same objects come out, with a warning.
    fn host_mentions(
        &mut self,
        bag: &Resolved,
        hosts_key: &str,
        groups_key: &str,
        named_hosts_stay: bool,
    ) -> Option<(Vec<String>, BTreeSet<String>)> {
        if bag.text(hosts_key).is_none() && bag.text(groups_key).is_none() {
            return None;
        }
        // Hosts and groups are only added, so a list of the same length is
        // the same list.
        if self.host_names.len() != self.out.hosts.len() {
            self.host_names = self.out.hosts.keys().cloned().collect();
        }
        if self.hostgroup_names.len() != self.out.hostgroups.len() {
            self.hostgroup_names = self.out.hostgroups.keys().cloned().collect();
        }
        let hosts = std::mem::take(&mut self.host_names);
        let groups = std::mem::take(&mut self.hostgroup_names);
        let mut host_items = bag.list(hosts_key);
        if named_hosts_stay
            && bag.text(groups_key).is_none()
            && host_items.iter().any(|item| item.starts_with('!'))
        {
            host_items.retain(|item| !item.starts_with('!'));
            self.warn(
                &bag.loc,
                format!(
                    "the '!' entries of {hosts_key} have no effect: Naemon ignores them in a host dependency, \
                     host escalation or hostextinfo unless {groups_key} is given too, and checkd does the same"
                ),
            );
        }
        let by_name = self.pick(&host_items, &hosts, "host", &bag.loc);
        let by_group = self.pick(&bag.list(groups_key), &groups, "hostgroup", &bag.loc);
        self.host_names = hosts;
        self.hostgroup_names = groups;
        let members = |names: &mut dyn Iterator<Item = &String>| -> Vec<String> {
            names
                .flat_map(|group| self.out.hostgroups[group].members.iter().cloned())
                .collect()
        };
        let mut exclude = by_name.exclude.clone();
        exclude.extend(members(&mut by_group.exclude.iter()));
        let mut all = by_name.mentions.clone();
        let mut from_groups = members(&mut by_group.mentions.iter());
        from_groups.retain(|host| !exclude.contains(host));
        if named_hosts_stay {
            let kept: BTreeSet<&String> = by_name
                .include
                .iter()
                .filter(|host| exclude.contains(*host))
                .collect();
            if !kept.is_empty() {
                let kept: Vec<&str> = kept.into_iter().map(String::as_str).collect();
                self.warn(
                    &bag.loc,
                    format!(
                        "{hosts_key} names '{}', which the '!' entries take out: Naemon keeps a host that \
                         {hosts_key} names in a host dependency, host escalation or hostextinfo, and checkd \
                         does the same",
                        kept.join("', '")
                    ),
                );
            }
        } else {
            all.retain(|host| !exclude.contains(host));
        }
        all.extend(from_groups);
        Some((all, by_name.include.into_iter().collect()))
    }

    /// Returns the groups each service asks to be in.
    ///
    /// A service can be defined more than once for one host. A definition
    /// that names the host in `host_name` wins over one that reaches it
    /// through a host group, in either order and with no warning, as in
    /// the reference.
    ///
    /// Two definitions of the same sort for one host are an error. The
    /// reference keeps one of them and warns, and which one it keeps
    /// differs from host to host (the corpus cases
    /// `bad-duplicate-service-*` show how). That is too hard to predict
    /// from the file to copy.
    fn services(&mut self, resolved: Vec<Resolved>) -> Asked<ServiceId> {
        let mut chosen: BTreeMap<ServiceId, Candidate> = BTreeMap::new();
        // One entry for each pair of definitions that clash: where the
        // later and the earlier one are, the description, and the hosts.
        let mut clashes: Vec<(Loc, Loc, String, Vec<String>)> = Vec::new();
        // Where each service was first defined for a host, by host name
        // (`true`) and through a host group (`false`).
        let mut seen: BTreeMap<(ServiceId, bool), Loc> = BTreeMap::new();
        for bag in resolved {
            let description = bag.name("service_description");
            // `null` for both host settings leaves the service without hosts.
            let gives_hosts =
                bag.text("host_name").is_some() || bag.text("hostgroup_name").is_some();
            let hosts = match (&description, gives_hosts) {
                (Some(_), true) => self.host_set(&bag, "host_name", "hostgroup_name", false),
                _ => None,
            };
            let (Some(description), Some((hosts, named))) = (description, hosts) else {
                self.nameless(
                    &bag,
                    "service",
                    "service_description and host_name or hostgroup_name",
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
                // Two definitions of the same sort clash, also when a
                // third that names the host would be used over both.
                match seen.get(&(id.clone(), candidate.by_host_name)) {
                    Some(earlier) => {
                        match clashes.iter_mut().find(|(later, first, text, _)| {
                            *later == bag.loc && first == earlier && *text == description
                        }) {
                            Some((_, _, _, hosts)) => hosts.push(host),
                            None => clashes.push((
                                bag.loc.clone(),
                                earlier.clone(),
                                description.clone(),
                                vec![host],
                            )),
                        }
                        continue;
                    }
                    None => {
                        seen.insert((id.clone(), candidate.by_host_name), bag.loc.clone());
                    }
                }
                match chosen.get(&id) {
                    Some(earlier) if earlier.by_host_name => {}
                    _ => {
                        chosen.insert(id, candidate);
                    }
                }
            }
        }

        for (later, earlier, description, mut hosts) in clashes {
            hosts.sort();
            let shown = hosts
                .iter()
                .take(3)
                .map(|host| format!("'{host}'"))
                .collect::<Vec<_>>()
                .join(", ");
            let more = match hosts.len().saturating_sub(3) {
                0 => String::new(),
                rest => format!(" and {rest} more"),
            };
            self.error(
                &later,
                format!(
                    "service '{description}' is defined twice for host {shown}{more}: here and at \
                     {earlier}. Naemon keeps one of the two; checkd refuses this"
                ),
            );
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
            memberships.push((bag.list("servicegroups"), id.clone(), bag.loc.clone()));
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

    /// What a service escalation, or one side of a service dependency,
    /// gives to say which services it is about. `keys` are the settings
    /// for hosts, host groups, service groups and descriptions.
    fn side(&self, bag: &Resolved, keys: [&str; 4]) -> Side {
        let [hosts, hostgroups, servicegroups, descriptions] = keys;
        let hosts = bag.text(hosts).is_some() || bag.text(hostgroups).is_some();
        let groups = bag.text(servicegroups).is_some();
        match (hosts, bag.text(descriptions).is_some(), groups) {
            (false, false, false) => Side::Nothing,
            (false, true, false) => Side::DescriptionOnly,
            (true, true, _) | (false, false, true) => Side::Named,
            (true, false, _) => Side::Faulty("hosts and no service description"),
            (false, true, true) => {
                Side::Faulty("a service description with a service group and no host")
            }
        }
    }

    /// The services a dependency or escalation names, from host lists,
    /// host group lists, a service group list and a description list.
    ///
    /// An exclusion takes services out of the whole result, not only out
    /// of its own list: `!group` takes out every member of that service
    /// group, and `!description` takes that service out on each host the
    /// definition names, also where a service group brought it in. An
    /// excluded host does not reach into the service groups.
    fn service_set(&mut self, bag: &Resolved, keys: [&str; 4]) -> Vec<ServiceId> {
        let [hosts_key, groups_key, servicegroups_key, descriptions_key] = keys;
        let mut found = Vec::new();
        let mut taken_out: BTreeSet<ServiceId> = BTreeSet::new();
        let servicegroups: Vec<String> = self.out.servicegroups.keys().cloned().collect();
        let picked = self.pick(
            &bag.list(servicegroups_key),
            &servicegroups.iter().map(String::as_str).collect::<Vec<_>>(),
            "servicegroup",
            &bag.loc,
        );
        for group in &picked.exclude {
            taken_out.extend(self.out.servicegroups[group].members.iter().cloned());
        }
        for group in picked.kept() {
            for member in &self.out.servicegroups[&group].members {
                push_unique(&mut found, member.clone());
            }
        }
        if let Some((hosts, _)) = self.host_set(bag, hosts_key, groups_key, false) {
            for host in hosts {
                let picked = self.descriptions_on(bag, &host, descriptions_key);
                for description in picked.exclude {
                    taken_out.insert((host.clone(), description));
                }
                for description in picked.include {
                    push_unique(&mut found, (host.clone(), description));
                }
            }
        }
        found.retain(|service| !taken_out.contains(service));
        found
    }

    /// Expands a description list against the services of one host.
    fn descriptions_on(&mut self, bag: &Resolved, host: &str, descriptions_key: &str) -> Picked {
        let on_host: Vec<String> = self
            .out
            .services
            .range((host.to_string(), String::new())..)
            .take_while(|((h, _), _)| h == host)
            .map(|((_, description), _)| description.clone())
            .collect();
        let universe: Vec<&str> = on_host.iter().map(String::as_str).collect();
        let what = format!("on host '{host}', service");
        self.pick_from(
            &bag.list(descriptions_key),
            &universe,
            &what,
            &bag.loc,
            true,
        )
    }

    // ---- extended info ----

    /// The hosts an extended-info definition names, or `None` when it is
    /// to be skipped.
    fn extinfo_hosts(&mut self, bag: &Resolved, hostextinfo: bool) -> Option<Vec<String>> {
        let what = if hostextinfo {
            "hostextinfo"
        } else {
            "serviceextinfo"
        };
        let unregistered = !bag.registered;
        let complete = hostextinfo || bag.is_set("service_description");
        let hosts = match self.host_set(bag, "host_name", "hostgroup_name", hostextinfo) {
            Some((hosts, _)) if complete => hosts,
            _ => {
                // A template that only holds values for others is fine.
                if !unregistered {
                    let needs = if hostextinfo {
                        "no host_name and no hostgroup_name"
                    } else {
                        "no service_description, or no host_name and no hostgroup_name"
                    };
                    self.warn(
                        &bag.loc,
                        format!("this {what} gives {needs}, so it has no effect"),
                    );
                }
                return None;
            }
        };
        if unregistered {
            self.warn(
                &bag.loc,
                format!("'register 0' does not switch a {what} off: Naemon applies it all the same, and checkd does too"),
            );
        }
        if hosts.is_empty() && !self.diags.has_errors() {
            // Naemon refuses the one and accepts the other.
            if hostextinfo {
                self.error(&bag.loc, format!("this {what} applies to no host"));
            } else {
                self.warn(
                    &bag.loc,
                    format!("this {what} applies to no host, so it has no effect"),
                );
            }
        }
        Some(hosts)
    }

    /// Applies `hostextinfo` definitions: an older way to give a host its
    /// notes, links, images and coordinates. A value the host has itself,
    /// or from its templates, stays. Among several definitions for one
    /// host the one read last wins, so they are applied from the end and
    /// each fills only what is still empty.
    fn host_extinfo(&mut self, resolved: Vec<Resolved>) {
        for bag in resolved.into_iter().rev() {
            let Some(hosts) = self.extinfo_hosts(&bag, true) else {
                continue;
            };
            for name in hosts {
                let Some(host) = self.out.hosts.get_mut(&name) else {
                    continue;
                };
                fill_extinfo(&mut host.check, &bag);
                fill(&mut host.vrml_image, bag.owned("vrml_image"));
                fill(&mut host.statusmap_image, bag.owned("statusmap_image"));
                fill(
                    &mut host.coords_2d,
                    bag.text("2d_coords")
                        .and_then(|value| values::parse_coords2(value).ok()),
                );
                fill(
                    &mut host.coords_3d,
                    bag.text("3d_coords")
                        .and_then(|value| values::parse_coords3(value).ok()),
                );
            }
        }
    }

    /// The same for `serviceextinfo`. Every host named must have every
    /// service named.
    fn service_extinfo(&mut self, resolved: Vec<Resolved>) {
        for bag in resolved.into_iter().rev() {
            let Some(hosts) = self.extinfo_hosts(&bag, false) else {
                continue;
            };
            for host in hosts {
                for description in self
                    .descriptions_on(&bag, &host, "service_description")
                    .kept()
                {
                    if let Some(service) = self.out.services.get_mut(&(host.clone(), description)) {
                        fill_extinfo(&mut service.check, &bag);
                    }
                }
            }
        }
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
        // With allow_empty_hostgroup_assignment off, Naemon refuses a host
        // dependency with a side that comes to no host, and it looks at
        // the ones with `register 0` too.
        let strict = !self.settings.allow_empty_hostgroups;
        for bag in resolved {
            if !bag.registered && !strict {
                continue;
            }
            // A name that is not defined, or a pattern that is not valid,
            // is an error anywhere else. Here Naemon prints the error,
            // drops the dependency and accepts the configuration, unless
            // the setting is off.
            self.held_back = Some(Vec::new());
            let masters = self.host_set(&bag, "host_name", "hostgroup_name", true);
            let dependents = self.host_set(
                &bag,
                "dependent_host_name",
                "dependent_hostgroup_name",
                true,
            );
            let faults = self.held_back.take().unwrap_or_default();
            if strict && !faults.is_empty() {
                for fault in faults {
                    self.error(&bag.loc, fault);
                }
                continue;
            }
            if let Some(first) = faults.first() {
                self.warn(
                    &bag.loc,
                    format!(
                        "this hostdependency has no effect and is dropped: {first}. Naemon drops it and accepts \
                         the configuration, and checkd does the same"
                    ),
                );
                continue;
            }
            // With neither side it is dropped whatever the setting says.
            let one_sided = masters.is_some() != dependents.is_some();
            let (Some((masters, _)), Some((dependents, _))) = (masters, dependents) else {
                if strict && one_sided {
                    self.error(
                        &bag.loc,
                        "this hostdependency lacks a host or a dependent host, and \
                         allow_empty_hostgroup_assignment is off",
                    );
                } else if bag.registered {
                    self.warn(
                        &bag.loc,
                        "this hostdependency lacks a host or a dependent host, so it has no effect and is dropped",
                    );
                }
                continue;
            };
            if strict && (masters.is_empty() || dependents.is_empty()) {
                let side = if masters.is_empty() {
                    "hosts"
                } else {
                    "dependent hosts"
                };
                self.error(
                    &bag.loc,
                    format!(
                        "the {side} of this hostdependency come to no host, and \
                         allow_empty_hostgroup_assignment is off"
                    ),
                );
                continue;
            }
            if !bag.registered {
                continue;
            }
            let pairs = masters
                .iter()
                .flat_map(|m| dependents.iter().map(move |d| (m.clone(), d.clone())))
                .collect();
            let built = self.dependency(&bag, schema::HOST_FAILURE, pairs);
            self.out.host_dependencies.extend(built);
        }
    }

    /// Service dependencies. Each side names its services with hosts and
    /// a description, with a service group, or with both. A side that
    /// gives only a description means "on the same host as the service on
    /// the other side". Found by asking the reference:
    ///
    ///   - a dependency that names no dependent service at all is refused,
    ///     and so is one that gives only descriptions on both sides;
    ///   - one that names no service to depend on is accepted and makes
    ///     nothing;
    ///   - hosts with no description are refused, and so is a description
    ///     next to a service group with no hosts.
    fn service_dependencies(&mut self, resolved: Vec<Resolved>) {
        const MASTER: [&str; 4] = [
            "host_name",
            "hostgroup_name",
            "servicegroup_name",
            "service_description",
        ];
        const DEPENDENT: [&str; 4] = [
            "dependent_host_name",
            "dependent_hostgroup_name",
            "dependent_servicegroup_name",
            "dependent_service_description",
        ];
        for bag in resolved {
            let master = self.side(&bag, MASTER);
            let dependent = self.side(&bag, DEPENDENT);
            let mut pairs = Vec::new();
            match (master, dependent) {
                (Side::Faulty(given), _) => {
                    self.error(
                        &bag.loc,
                        format!("this servicedependency gives {given} for the service depended on"),
                    );
                    continue;
                }
                (_, Side::Faulty(given)) => {
                    self.error(
                        &bag.loc,
                        format!("this servicedependency gives {given} for the dependent service"),
                    );
                    continue;
                }
                (_, Side::Nothing) => {
                    self.error(
                        &bag.loc,
                        "this servicedependency names no dependent service",
                    );
                    continue;
                }
                (Side::Nothing | Side::DescriptionOnly, Side::DescriptionOnly) => {
                    self.error(
                        &bag.loc,
                        "this servicedependency names no host on either side",
                    );
                    continue;
                }
                (Side::Nothing, Side::Named) => {
                    // Looked up all the same, so that an unknown name in
                    // it is reported.
                    self.service_set(&bag, DEPENDENT);
                    self.warn(
                        &bag.loc,
                        "this servicedependency names no service to depend on, so it has no effect and is dropped",
                    );
                    continue;
                }
                (Side::Named, Side::Named) => {
                    let masters = self.service_set(&bag, MASTER);
                    let dependents = self.service_set(&bag, DEPENDENT);
                    for master in &masters {
                        for dependent in &dependents {
                            pairs.push((master.clone(), dependent.clone()));
                        }
                    }
                }
                (Side::Named, Side::DescriptionOnly) => {
                    for master in self.service_set(&bag, MASTER) {
                        for description in
                            self.descriptions_on(&bag, &master.0, DEPENDENT[3]).kept()
                        {
                            pairs.push((master.clone(), (master.0.clone(), description)));
                        }
                    }
                }
                (Side::DescriptionOnly, Side::Named) => {
                    for dependent in self.service_set(&bag, DEPENDENT) {
                        for description in
                            self.descriptions_on(&bag, &dependent.0, MASTER[3]).kept()
                        {
                            pairs.push(((dependent.0.clone(), description), dependent.clone()));
                        }
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
            let Some((hosts, _)) = self.host_mentions(&bag, "host_name", "hostgroup_name", true)
            else {
                self.error(
                    &bag.loc,
                    "a hostescalation needs a host_name or hostgroup_name",
                );
                continue;
            };
            if hosts.is_empty() && !self.diags.has_errors() {
                self.error(&bag.loc, "this hostescalation applies to no host");
            }
            // One escalation each time a host comes up, as Naemon makes
            // them. Dependencies and services are made once per host.
            let mut times: BTreeMap<&String, usize> = BTreeMap::new();
            for host in &hosts {
                *times.entry(host).or_default() += 1;
            }
            let repeated: Vec<String> = times
                .iter()
                .filter(|(_, n)| **n > 1)
                .map(|(host, n)| format!("'{host}' {n} times"))
                .collect();
            if !repeated.is_empty() {
                self.warn(
                    &bag.loc,
                    format!(
                        "the lists of this hostescalation bring up {}: Naemon makes one escalation each \
                         time, and checkd does the same",
                        repeated.join(", ")
                    ),
                );
            }
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
            const KEYS: [&str; 4] = [
                "host_name",
                "hostgroup_name",
                "servicegroup_name",
                "service_description",
            ];
            // Unlike a service dependency, an escalation has no other
            // side to take hosts from.
            match self.side(&bag, KEYS) {
                Side::Named => {}
                Side::Nothing => {
                    self.error(&bag.loc, "this serviceescalation names no service");
                    continue;
                }
                Side::DescriptionOnly => {
                    self.error(
                        &bag.loc,
                        "this serviceescalation gives a service description and no host",
                    );
                    continue;
                }
                Side::Faulty(given) => {
                    self.error(&bag.loc, format!("this serviceescalation gives {given}"));
                    continue;
                }
            }
            let services = self.service_set(&bag, KEYS);
            if services.is_empty() && !self.diags.has_errors() {
                self.error(&bag.loc, "this serviceescalation applies to no service");
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
