//! Reads object files into definitions, before templates are applied.

use std::path::PathBuf;
use std::sync::Arc;

use crate::diag::{Diagnostics, Loc};

/// The object types, in the order the resolved-object dump writes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObjType {
    Timeperiod,
    Command,
    ContactGroup,
    HostGroup,
    ServiceGroup,
    Contact,
    Host,
    Service,
    ServiceDependency,
    ServiceEscalation,
    HostDependency,
    HostEscalation,
    /// Not objects of their own: they add settings to hosts and services,
    /// and the dump never writes them.
    HostExtInfo,
    ServiceExtInfo,
}

impl ObjType {
    pub const ALL: [ObjType; 14] = [
        ObjType::Timeperiod,
        ObjType::Command,
        ObjType::ContactGroup,
        ObjType::HostGroup,
        ObjType::ServiceGroup,
        ObjType::Contact,
        ObjType::Host,
        ObjType::Service,
        ObjType::ServiceDependency,
        ObjType::ServiceEscalation,
        ObjType::HostDependency,
        ObjType::HostEscalation,
        ObjType::HostExtInfo,
        ObjType::ServiceExtInfo,
    ];

    /// The word after `define`.
    pub fn keyword(self) -> &'static str {
        match self {
            ObjType::Timeperiod => "timeperiod",
            ObjType::Command => "command",
            ObjType::ContactGroup => "contactgroup",
            ObjType::HostGroup => "hostgroup",
            ObjType::ServiceGroup => "servicegroup",
            ObjType::Contact => "contact",
            ObjType::Host => "host",
            ObjType::Service => "service",
            ObjType::ServiceDependency => "servicedependency",
            ObjType::ServiceEscalation => "serviceescalation",
            ObjType::HostDependency => "hostdependency",
            ObjType::HostEscalation => "hostescalation",
            ObjType::HostExtInfo => "hostextinfo",
            ObjType::ServiceExtInfo => "serviceextinfo",
        }
    }

    /// Whether this is one of the two extended-info types.
    pub fn is_extinfo(self) -> bool {
        matches!(self, ObjType::HostExtInfo | ObjType::ServiceExtInfo)
    }

    pub fn from_keyword(word: &str) -> Option<ObjType> {
        ObjType::ALL.into_iter().find(|t| t.keyword() == word)
    }
}

/// One `name value` line of a definition.
#[derive(Clone, Debug)]
pub struct Attr {
    pub key: String,
    pub value: String,
    pub line: u32,
}

/// One `define ... { }` block as written.
#[derive(Clone, Debug)]
pub struct RawObject {
    pub kind: ObjType,
    pub attrs: Vec<Attr>,
    pub loc: Loc,
}

impl RawObject {
    /// The value of a setting. When it is given more than once the last
    /// one counts.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .rev()
            .find(|a| a.key == key)
            .map(|a| a.value.as_str())
    }
}

/// Settings of a timeperiod that are not day or date lines.
pub const TIMEPERIOD_FIXED: [&str; 6] = [
    "timeperiod_name",
    "alias",
    "exclude",
    "name",
    "use",
    "register",
];

/// Joins continuation lines. Returns each logical line with the number of
/// the physical line it starts on.
fn logical_lines(text: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let mut pending: Option<(u32, String)> = None;
    for (i, physical) in text.split('\n').enumerate() {
        let physical = physical.strip_suffix('\r').unwrap_or(physical);
        let (start, mut line) = match pending.take() {
            Some((start, mut joined)) => {
                joined.push_str(physical.trim_start());
                (start, joined)
            }
            None => (i as u32 + 1, physical.to_string()),
        };
        let is_comment = matches!(line.trim_start().chars().next(), Some('#' | ';'));
        if !is_comment && line.ends_with('\\') {
            line.pop();
            pending = Some((start, line));
        } else {
            out.push((start, line));
        }
    }
    if let Some(last) = pending {
        out.push(last);
    }
    out
}

/// Cuts a `;` comment off a line and turns `\;` into `;`.
fn strip_comment(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&';') => {
                chars.next();
                out.push(';');
            }
            ';' => break,
            _ => out.push(c),
        }
    }
    out
}

/// Parses one object file. Definitions are added to `out`.
pub fn parse_file(
    file: &Arc<PathBuf>,
    text: &str,
    out: &mut Vec<RawObject>,
    diags: &mut Diagnostics,
) {
    let mut current: Option<RawObject> = None;
    // Set inside a definition whose first line was faulty, so that its
    // settings are not each reported as being outside a definition.
    let mut skipping = false;
    for (line_no, line) in logical_lines(text) {
        let loc = Loc {
            file: file.clone(),
            line: line_no,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        let stripped = strip_comment(trimmed);
        let line = stripped.trim();
        if line.is_empty() {
            continue;
        }
        let (word, rest) = match line.find(char::is_whitespace) {
            Some(at) => (&line[..at], line[at..].trim()),
            None => (line, ""),
        };

        if let Some(object) = current.as_mut() {
            if line == "}" {
                out.extend(current.take());
            } else if let Some(rest) = line.strip_prefix('}') {
                // Only a `;` comment may follow the brace. Naemon reads
                // anything else as a setting called `}` and refuses it.
                diags.error(
                    Some(&loc),
                    format!(
                        "'{}' after the closing brace; only a ';' comment may follow it",
                        rest.trim()
                    ),
                );
                out.extend(current.take());
            } else if word == "define" || word.starts_with("define{") {
                diags.error(
                    Some(&loc),
                    format!(
                        "a new definition starts here, but the {} definition from line {} has no closing brace",
                        object.kind.keyword(),
                        object.loc.line
                    ),
                );
                // Carry on with the new definition so later faults show too.
                current = start_definition(rest, word, &loc, diags);
                skipping = current.is_none();
            } else if object.kind == ObjType::Timeperiod && !TIMEPERIOD_FIXED.contains(&word) {
                // A day or date line. The times are the last word, and
                // everything before them says which days.
                let (days, times) = match line.rfind(char::is_whitespace) {
                    Some(at) => (&line[..at], &line[at + 1..]),
                    None => (line, ""),
                };
                let days = days.split_whitespace().collect::<Vec<_>>().join(" ");
                object.attrs.push(Attr {
                    key: days,
                    value: times.to_string(),
                    line: line_no,
                });
            } else {
                let key = if word.starts_with('_') {
                    word.to_uppercase()
                } else {
                    word.to_string()
                };
                object.attrs.push(Attr {
                    key,
                    value: rest.to_string(),
                    line: line_no,
                });
            }
        } else if word == "define" || word.starts_with("define{") {
            current = start_definition(rest, word, &loc, diags);
            skipping = current.is_none();
        } else if skipping {
            skipping = !line.starts_with('}');
        } else if line.starts_with('}') {
            diags.error(Some(&loc), "closing brace with no definition open");
        } else {
            diags.error(Some(&loc), format!("'{word}' is outside any definition"));
        }
    }
    if let Some(object) = current {
        diags.error(
            Some(&object.loc),
            format!(
                "the {} definition that starts here has no closing brace",
                object.kind.keyword()
            ),
        );
    }
}

/// Handles a `define <type> {` line. `rest` is what follows the first word.
fn start_definition(
    rest: &str,
    word: &str,
    loc: &Loc,
    diags: &mut Diagnostics,
) -> Option<RawObject> {
    // "define{" with no type is covered by the empty-name error below.
    let after = if word == "define" {
        rest
    } else {
        &word["define".len()..]
    };
    let Some((name, extra)) = after.split_once('{') else {
        diags.error(
            Some(loc),
            "a definition must start with 'define <type> {' on one line",
        );
        return None;
    };
    if !extra.trim().is_empty() {
        diags.warn(
            Some(loc),
            format!(
                "'{}' after the opening brace is not read; Naemon drops it too",
                extra.trim()
            ),
        );
    }
    let name = name.trim();
    match ObjType::from_keyword(name) {
        Some(kind) => Some(RawObject {
            kind,
            attrs: Vec::new(),
            loc: loc.clone(),
        }),
        None => {
            diags.error(Some(loc), format!("unknown object type '{name}'"));
            None
        }
    }
}
