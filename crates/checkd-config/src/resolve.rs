//! Applies templates: `use`, `null` and a leading `+`.

use std::collections::{BTreeSet, HashMap};

use crate::diag::{Diagnostics, Loc};
use crate::raw::{ObjType, RawObject, TIMEPERIOD_FIXED};
use crate::schema;
use crate::values::split_list;

/// A definition with everything it inherits filled in. Values are still
/// text. A value of `null` and a leading `+` are kept, because what they
/// mean depends on what is done with the setting next.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub attrs: Vec<(String, String)>,
    pub loc: Loc,
    /// The definition's own `name`, by which others `use` it.
    pub template_name: Option<String>,
    /// False when the definition itself says `register 0`.
    pub registered: bool,
}

impl Resolved {
    /// The value as resolved. Standard settings appear once.
    pub fn raw(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Whether the definition or one of its templates gives this setting,
    /// `null` included.
    pub fn is_set(&self, key: &str) -> bool {
        self.raw(key).is_some()
    }

    /// The value, or `None` when it is not given or is cancelled by `null`.
    pub fn text(&self, key: &str) -> Option<&str> {
        self.raw(key).filter(|value| *value != "null")
    }

    /// The items of a list setting, without a leading `+`.
    pub fn list(&self, key: &str) -> Vec<String> {
        self.text(key)
            .map(|value| split_list(value.strip_prefix('+').unwrap_or(value)))
            .unwrap_or_default()
    }

    /// Whether a list setting still has its leading `+` after templates.
    pub fn is_additive(&self, key: &str) -> bool {
        self.raw(key).is_some_and(|value| value.starts_with('+'))
    }

    pub fn set(&mut self, key: &str, value: String) {
        match self.attrs.iter_mut().find(|(k, _)| k == key) {
            Some(entry) => entry.1 = value,
            None => self.attrs.push((key.to_string(), value)),
        }
    }

    /// Custom variables, names in upper case without the underscore.
    pub fn custom(&self) -> Vec<(String, String)> {
        self.attrs
            .iter()
            .filter(|(k, _)| k.starts_with('_'))
            .map(|(k, v)| (k[1..].to_string(), v.clone()))
            .collect()
    }
}

struct Resolver<'a> {
    kind: ObjType,
    objects: &'a [RawObject],
    by_name: HashMap<&'a str, usize>,
}

impl Resolver<'_> {
    fn templates(&self, index: usize) -> Vec<usize> {
        let names = self.objects[index]
            .get("use")
            .map(split_list)
            .unwrap_or_default();
        names
            .iter()
            .filter_map(|name| self.by_name.get(name.as_str()).copied())
            .collect()
    }

    /// Looks a setting up in a definition and then in its templates, depth
    /// first and left to right. `path` holds the definitions being looked
    /// at, so a template loop ends instead of going round.
    fn lookup(&self, index: usize, key: &str, path: &mut Vec<usize>) -> Option<String> {
        if path.contains(&index) {
            return None;
        }
        path.push(index);
        let own = self.objects[index].get(key);
        let additive = !key.starts_with('_') && schema::is_list(self.kind, key);
        let found = match own {
            Some(value) if additive && value.starts_with('+') => {
                match self.inherited(index, key, path) {
                    // The inherited value may itself start with "+", when its
                    // own chain ran out. The result then still does.
                    Some(below) if below == "null" => Some(value.to_string()),
                    Some(below) => match below.strip_prefix('+') {
                        Some(rest) => Some(format!("{value},{rest}")),
                        None => Some(format!("{},{below}", &value[1..])),
                    },
                    None => Some(value.to_string()),
                }
            }
            Some(value) => Some(value.to_string()),
            None => self.inherited(index, key, path),
        };
        path.pop();
        found
    }

    fn inherited(&self, index: usize, key: &str, path: &mut Vec<usize>) -> Option<String> {
        self.templates(index)
            .into_iter()
            .find_map(|template| self.lookup(template, key, path))
    }

    /// Every definition reachable through `use`, depth first, the starting
    /// one included.
    fn ancestry(&self, index: usize, seen: &mut Vec<usize>) {
        if seen.contains(&index) {
            return;
        }
        seen.push(index);
        for template in self.templates(index) {
            self.ancestry(template, seen);
        }
    }

    /// The day and date lines of a timeperiod, in the order the reference
    /// writes them: its own in the order written, then those of each
    /// template with that template's order turned round. A template's order
    /// is worked out the same way, so two levels of templates turn the
    /// order round twice.
    fn day_lines(&self, index: usize, path: &mut Vec<usize>) -> Vec<String> {
        if path.contains(&index) {
            return Vec::new();
        }
        path.push(index);
        let mut order: Vec<String> = Vec::new();
        for attr in &self.objects[index].attrs {
            let key = attr.key.as_str();
            if !TIMEPERIOD_FIXED.contains(&key)
                && !key.starts_with('_')
                && !order.iter().any(|k| k == key)
            {
                order.push(attr.key.clone());
            }
        }
        for template in self.templates(index) {
            for key in self.day_lines(template, path).into_iter().rev() {
                if !order.contains(&key) {
                    order.push(key);
                }
            }
        }
        path.pop();
        order
    }

    fn resolve(&self, index: usize) -> Resolved {
        let object = &self.objects[index];
        let mut attrs: Vec<(String, String)> = Vec::new();
        let mut line = Vec::new();
        self.ancestry(index, &mut line);
        for (depth, &from) in line.iter().enumerate() {
            for attr in &self.objects[from].attrs {
                let key = attr.key.as_str();
                if matches!(key, "name" | "use" | "register") {
                    continue;
                }
                let is_custom = key.starts_with('_');
                if depth == 0 && is_custom {
                    // The same custom variable twice in one definition is
                    // kept twice, as the reference does.
                    attrs.push((attr.key.clone(), attr.value.clone()));
                } else if !attrs.iter().any(|(k, _)| k == key)
                    && let Some(value) = self.lookup(index, key, &mut Vec::new())
                {
                    attrs.push((attr.key.clone(), value));
                }
            }
        }
        if self.kind == ObjType::Timeperiod {
            // Day and date lines come in the order the reference writes
            // them. Everything else keeps its place.
            let order = self.day_lines(index, &mut Vec::new());
            let (mut days, mut rest): (Vec<_>, Vec<_>) =
                attrs.into_iter().partition(|(key, _)| order.contains(key));
            days.sort_by_key(|(key, _)| order.iter().position(|k| k == key));
            rest.append(&mut days);
            attrs = rest;
        }
        // A service template can force its check command on the services
        // that use it by writing "!" in front of it.
        if self.kind == ObjType::Service {
            let forced = line[1..].iter().find_map(|&from| {
                self.objects[from]
                    .get("check_command")
                    .and_then(|value| value.strip_prefix('!'))
            });
            if let Some(command) = forced {
                match attrs.iter_mut().find(|(k, _)| k == "check_command") {
                    Some(entry) => entry.1 = command.to_string(),
                    None => attrs.push(("check_command".to_string(), command.to_string())),
                }
            }
        }
        Resolved {
            attrs,
            loc: object.loc.clone(),
            template_name: object.get("name").map(String::from),
            registered: object.get("register") != Some("0"),
        }
    }
}

/// Resolves the definitions of one type. Returns the registered ones, in
/// file order.
pub fn resolve(kind: ObjType, objects: &[RawObject], diags: &mut Diagnostics) -> Vec<Resolved> {
    let mut by_name: HashMap<&str, usize> = HashMap::new();
    for (index, object) in objects.iter().enumerate() {
        let Some(name) = object.get("name") else {
            continue;
        };
        if let Some(first) = by_name.insert(name, index) {
            by_name.insert(name, first);
            diags.error(
                Some(&object.loc),
                format!(
                    "{} template '{name}' is already defined at {}",
                    kind.keyword(),
                    objects[first].loc
                ),
            );
        }
    }
    let resolver = Resolver {
        kind,
        objects,
        by_name,
    };

    let mut looping = BTreeSet::new();
    for (index, object) in objects.iter().enumerate() {
        for name in object.get("use").map(split_list).unwrap_or_default() {
            if !resolver.by_name.contains_key(name.as_str()) {
                diags.error(
                    Some(&object.loc),
                    format!("{} template '{name}' is not defined", kind.keyword()),
                );
            }
        }
        // A definition that can reach itself through "use" is in a loop.
        let mut reachable = Vec::new();
        for template in resolver.templates(index) {
            resolver.ancestry(template, &mut reachable);
        }
        if reachable.contains(&index) && looping.insert(index) {
            diags.warn(
                Some(&object.loc),
                format!(
                    "{} template '{}' uses itself, directly or through other templates; the loop adds nothing",
                    kind.keyword(),
                    object.get("name").unwrap_or("?")
                ),
            );
        }
    }

    (0..objects.len())
        // The reference applies an extended-info definition even when it
        // says `register 0`, so those are kept here too.
        .filter(|&index| kind.is_extinfo() || objects[index].get("register") != Some("0"))
        .map(|index| resolver.resolve(index))
        .collect()
}
