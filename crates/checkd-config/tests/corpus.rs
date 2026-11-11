//! Loads every case of the corpus and compares the result with what the
//! reference Naemon made of the same files.
//!
//! Compared: whether the configuration is accepted, and the resolved
//! objects. Not compared: the wording of messages, and order where order
//! has no meaning (see `normalise`).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use checkd_config::maincfg::PathMap;
use checkd_config::{Options, dump, load};

type Object = (String, Vec<(String, String)>);

/// Object types of which the reference dump has several entries with the
/// same names, in an order that carries no meaning.
const UNORDERED_TYPES: [&str; 4] = [
    "servicedependency",
    "serviceescalation",
    "hostdependency",
    "hostescalation",
];

/// Settings whose value is a set of names.
const SET_KEYS: [&str; 8] = [
    "members",
    "contacts",
    "contact_groups",
    "parents",
    "exclude",
    "service_notification_commands",
    "host_notification_commands",
    "exceptions",
];

fn parse_dump(text: &str) -> Vec<Object> {
    let mut objects = Vec::new();
    let mut current: Option<Object> = None;
    for line in text.lines() {
        if let Some(kind) = line
            .strip_prefix("define ")
            .and_then(|rest| rest.strip_suffix(" {"))
        {
            current = Some((kind.to_string(), Vec::new()));
        } else if line == "\t}" {
            objects.extend(current.take());
        } else if let (Some(rest), Some(object)) = (line.strip_prefix('\t'), current.as_mut()) {
            let (key, value) = rest.split_once('\t').unwrap_or((rest, ""));
            object.1.push((key.to_string(), value.to_string()));
        }
    }
    objects
}

/// Removes differences that carry no meaning.
fn normalise(mut objects: Vec<Object>) -> Vec<Object> {
    for (kind, attrs) in &mut objects {
        // Naemon always writes `o` here, whatever the configuration says.
        attrs.retain(|(key, _)| key != "initial_state");
        for (key, value) in attrs.iter_mut() {
            if !SET_KEYS.contains(&key.as_str()) {
                continue;
            }
            let items: Vec<&str> = value.split(',').collect();
            let mut items: Vec<String> = if kind == "servicegroup" && key == "members" {
                items.chunks(2).map(|pair| pair.join(",")).collect()
            } else {
                items.into_iter().map(String::from).collect()
            };
            items.sort();
            *value = items.join(",");
        }
        // Custom variables come last; their order among themselves is
        // an accident of how the reference stores them.
        let first_custom = attrs
            .iter()
            .position(|(key, _)| key.starts_with('_'))
            .unwrap_or(attrs.len());
        attrs[first_custom..].sort();
    }
    let (mut unordered, ordered): (Vec<Object>, Vec<Object>) = objects
        .into_iter()
        .partition(|(kind, _)| UNORDERED_TYPES.contains(&kind.as_str()));
    unordered.sort();
    ordered.into_iter().chain(unordered).collect()
}

fn render((kind, attrs): &Object) -> String {
    let naming: Vec<&str> = attrs
        .iter()
        .filter(|(key, _)| key.ends_with("_name") || key.ends_with("service_description"))
        .map(|(_, value)| value.as_str())
        .collect();
    format!("{kind} [{}]", naming.join(" / "))
}

/// Describes how two dumps differ, in a few lines.
fn difference(ours: &[Object], theirs: &[Object]) -> Vec<String> {
    let mut lines = Vec::new();
    let ours_set: BTreeSet<&Object> = ours.iter().collect();
    let theirs_set: BTreeSet<&Object> = theirs.iter().collect();
    let only_ours: Vec<&&Object> = ours_set.difference(&theirs_set).collect();
    let only_theirs: Vec<&&Object> = theirs_set.difference(&ours_set).collect();
    let mut matched = BTreeSet::new();
    for object in &only_ours {
        // The same object with other settings, if there is exactly one.
        let twins: Vec<&&&Object> = only_theirs
            .iter()
            .filter(|other| render(other) == render(object))
            .collect();
        match twins.as_slice() {
            [twin] => {
                matched.insert(render(twin));
                lines.push(format!("  differs: {}", render(object)));
                let (a, b): (BTreeSet<_>, BTreeSet<_>) =
                    (object.1.iter().collect(), twin.1.iter().collect());
                for (key, value) in a.difference(&b) {
                    lines.push(format!("    checkd: {key} = {value:?}"));
                }
                for (key, value) in b.difference(&a) {
                    lines.push(format!("    naemon: {key} = {value:?}"));
                }
                if a == b {
                    lines.push("    same settings, in another order".to_string());
                    lines.push(format!(
                        "    checkd: {:?}",
                        object.1.iter().map(|(k, _)| k).collect::<Vec<_>>()
                    ));
                    lines.push(format!(
                        "    naemon: {:?}",
                        twin.1.iter().map(|(k, _)| k).collect::<Vec<_>>()
                    ));
                }
            }
            _ => lines.push(format!(
                "  only in checkd: {} {:?}",
                render(object),
                object.1
            )),
        }
    }
    for object in only_theirs
        .iter()
        .filter(|object| !matched.contains(&render(object)))
    {
        lines.push(format!(
            "  only in naemon: {} {:?}",
            render(object),
            object.1
        ));
    }
    if lines.is_empty() && ours != theirs {
        lines.push("  same objects, in another order or another number".to_string());
        lines.push(format!(
            "    checkd: {:?}",
            ours.iter().map(render).collect::<Vec<_>>()
        ));
        lines.push(format!(
            "    naemon: {:?}",
            theirs.iter().map(render).collect::<Vec<_>>()
        ));
    }
    lines
}

/// Cases that the reference accepts and checkd refuses on purpose, each
/// with a part of the error checkd has to give. The loader's README says
/// why for each.
const REFUSED_ON_PURPOSE: [(&str, &str); 14] = [
    ("bad-duplicate-service", "is defined twice for host"),
    (
        "bad-duplicate-service-first-host",
        "is defined twice for host",
    ),
    (
        "bad-duplicate-service-hostgroups",
        "is defined twice for host",
    ),
    ("bad-duplicate-service-mixed", "is defined twice for host"),
    (
        "bad-duplicate-service-patterns",
        "is defined twice for host",
    ),
    ("bad-escalation-negative-notification", "is below zero"),
    ("bad-include-loop", "includes itself"),
    ("bad-include-missing-file", "cannot read"),
    (
        "bad-include-unknown-setting",
        "is not a main configuration setting",
    ),
    ("bad-number-hex", "is not a number written in digits"),
    ("bad-number-inf", "is not a number written in digits"),
    ("bad-number-nan", "is not a number written in digits"),
    ("bad-number-too-large", "is too large"),
    ("bad-number-too-large-for-interval", "is too large"),
];

fn check_case(case: &Path) -> Vec<String> {
    let config = case.join("config");
    let expected = case.join("expected");
    let options = Options {
        path_map: PathMap(vec![(PathBuf::from("/case"), config.clone())]),
    };
    let loaded = load(&config.join("naemon.cfg"), &options);

    let exit = fs::read_to_string(expected.join("verify.exit")).expect("verify.exit");
    let accepted_there = exit.trim() == "0";
    let mut lines = Vec::new();
    let name = case.file_name().unwrap_or_default().to_string_lossy();
    // A real tree says so in a file of its own, `checkd-refuses`, which
    // holds a part of the text that every error has to have.
    if let Ok(reason) = fs::read_to_string(case.join("checkd-refuses")) {
        let reason = reason.trim();
        if !accepted_there {
            lines.push("  marked as refused on purpose, but naemon refuses it too".to_string());
        }
        if loaded.objects.is_some() {
            lines.push("  marked as refused on purpose, but checkd accepts it".to_string());
        }
        lines.extend(
            loaded
                .diags
                .errors
                .iter()
                .filter(|error| !error.message.contains(reason))
                .take(20)
                .map(|error| format!("    error without '{reason}': {error}")),
        );
        return lines;
    }
    if let Some((_, reason)) = REFUSED_ON_PURPOSE.iter().find(|(case, _)| *case == name) {
        if !accepted_there {
            lines.push("  listed as refused on purpose, but naemon refuses it too".to_string());
        }
        if loaded.objects.is_some() {
            lines.push("  listed as refused on purpose, but checkd accepts it".to_string());
        } else if !loaded
            .diags
            .errors
            .iter()
            .any(|error| error.message.contains(reason))
        {
            lines.push(format!("  no error with '{reason}'"));
            lines.extend(
                loaded
                    .diags
                    .errors
                    .iter()
                    .map(|error| format!("    error: {error}")),
            );
        }
        return lines;
    }
    match (&loaded.objects, accepted_there) {
        (Some(_), false) => {
            lines.push("  checkd accepts this configuration; naemon refuses it".to_string())
        }
        (None, true) => {
            lines.push("  checkd refuses this configuration; naemon accepts it".to_string());
            lines.extend(
                loaded
                    .diags
                    .errors
                    .iter()
                    .take(6)
                    .map(|error| format!("    error: {error}")),
            );
        }
        (Some(objects), true) => {
            let theirs =
                fs::read_to_string(expected.join("objects.precache")).expect("objects.precache");
            let ours = dump::dump(objects);
            lines = difference(
                &normalise(parse_dump(&ours)),
                &normalise(parse_dump(&theirs)),
            );
        }
        (None, false) => {}
    }
    lines
}

#[test]
fn corpus_agrees_with_the_reference() {
    // `real/run` points this at the real config trees instead.
    let corpus = match std::env::var_os("CHECKD_CORPUS_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"),
    };
    let only = std::env::var("CHECKD_CASE").ok();
    let mut cases: Vec<PathBuf> = fs::read_dir(&corpus)
        .expect("corpus directory")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.join("config").is_dir())
        .filter(|path| {
            only.as_deref()
                .is_none_or(|only| path.file_name().is_some_and(|name| name == only))
        })
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "no corpus cases found");

    let mut report = String::new();
    let mut failed = 0;
    for case in &cases {
        let lines = check_case(case);
        if !lines.is_empty() {
            failed += 1;
            report.push_str(&format!(
                "{}\n",
                case.file_name().unwrap_or_default().to_string_lossy()
            ));
            for line in lines.iter().take(40) {
                report.push_str(line);
                report.push('\n');
            }
            if lines.len() > 40 {
                report.push_str(&format!("  ... and {} more lines\n", lines.len() - 40));
            }
        }
    }
    assert!(
        failed == 0,
        "{failed} of {} cases differ from the reference:\n{report}",
        cases.len()
    );
}

/// Warnings about a line of the main config, where Naemon says nothing:
/// case, line of `naemon.cfg`, and a part of the text.
const MAIN_WARNINGS: &[(&str, u32, &str)] = &[
    (
        "empty-hostgroup-assignment-loose-value",
        8,
        "Naemon reads '2x' as on",
    ),
    ("main-loose-state", 6, "Naemon reads '3x' as 3"),
    ("main-loose-state", 7, "Naemon reads 'unknown' as 0"),
];

/// Where the reference accepts a config without saying anything about a
/// part that has no effect, checkd accepts it too and says so. Each row is
/// a case, the line of `objects.cfg` the warning points at (0 for a warning
/// about the config as a whole), and a part of its text.
const WARNINGS: &[(&str, u32, &str)] = &[
    ("bad-dependency-no-criteria", 15, "names no failure states"),
    (
        "bad-duplicate-service-hostgroups",
        96,
        "the lines are added together",
    ),
    (
        "bad-escalation-first-after-last",
        9,
        "covers no notification",
    ),
    ("bad-escalation-no-range", 9, "no first_notification"),
    ("bad-exclusion-leaves-nothing", 9, "applies to no host"),
    ("bad-host-no-check-command", 3, "has no check_command"),
    ("bad-host-no-contacts", 3, "has no contacts"),
    ("bad-main-no-object-files", 0, "names no object files"),
    ("bad-no-hosts", 0, "no hosts are defined"),
    ("bad-service-on-empty-hostgroup", 13, "applies to no host"),
    ("bad-setting-without-value", 7, "'notes' has no value"),
    ("bad-template-loop", 3, "uses itself"),
    ("bad-template-uses-itself", 3, "uses itself"),
    ("bad-timeperiod-day-twice", 7, "'monday' is given twice"),
    ("contact-defaults", 9, "no host_notification_period"),
    (
        "dependency-exclusions",
        45,
        "'!' entries of dependent_host_name have no effect",
    ),
    (
        "dependency-exclusions",
        59,
        "'!' entries of host_name have no effect",
    ),
    (
        "escalation-exclusions",
        45,
        "'!' entries of host_name have no effect",
    ),
    (
        "escalation-exclusions",
        61,
        "'!' entries of host_name have no effect",
    ),
    (
        "loose-numbers",
        8,
        "'3abc' is not a plain number; Naemon reads it as 3",
    ),
    (
        "loose-numbers",
        43,
        "'5 # five tries' is not a plain number; Naemon reads it as 5",
    ),
    (
        "loose-numbers",
        71,
        "'often' is not a plain number; Naemon reads it as 0",
    ),
    (
        "loose-numbers",
        85,
        "'.5.5' is not a plain number; Naemon reads it as 0.5",
    ),
    ("loose-times", 12, "Naemon reads it as 10:15-17:00"),
    ("loose-times", 17, "Naemon reads it as 09:00-24:00"),
    (
        "regexp-servicegroups",
        42,
        "the description 'Star suffix' is not used",
    ),
    ("setting-twice", 8, "'alias' is given twice"),
    ("setting-twice", 25, "'check_interval' is given twice"),
    (
        "define-line-forms",
        39,
        "after the opening brace is not read",
    ),
    (
        "extinfo-targets",
        144,
        "gives no host_name and no hostgroup_name",
    ),
    ("extinfo-targets", 153, "gives no service_description"),
    ("extinfo-templates", 82, "does not switch a hostextinfo off"),
    (
        "extinfo-templates",
        102,
        "does not switch a serviceextinfo off",
    ),
    (
        "hostdependency-dropped",
        14,
        "host 'nosuch' is not defined. Naemon drops it",
    ),
    (
        "hostdependency-dropped",
        33,
        "hostgroup 'nosuch' is not defined. Naemon drops it",
    ),
    (
        "hostdependency-one-sided",
        16,
        "lacks a host or a dependent host",
    ),
    (
        "hostescalation-repeated-hosts",
        21,
        "bring up 'alpha' 2 times",
    ),
    (
        "hostescalation-repeated-hosts",
        58,
        "'alpha' 3 times, 'beta' 2 times",
    ),
    ("lists-leave-nothing", 32, "matches no pair of services"),
    (
        "lists-leave-nothing",
        127,
        "adds nothing and takes nothing out",
    ),
    (
        "lists-leave-nothing",
        163,
        "this serviceextinfo applies to no host",
    ),
    ("named-hosts-stay", 73, "dependent_host_name names 'alpha'"),
    ("named-hosts-stay", 81, "host_name names 'alpha'"),
    (
        "regexp-hostdependency-dropped",
        12,
        "'*' does not stand for every name",
    ),
    (
        "servicedependency-sides",
        152,
        "names no service to depend on",
    ),
    ("settings-without-effect", 9, "has no effect"),
    (
        "templates-nameless",
        5,
        "host template 'web-defaults' has no host_name",
    ),
    (
        "templates-nameless",
        20,
        "service template 'no-hosts' has no service_description",
    ),
    (
        "templates-nameless",
        60,
        "contact template 'pager-defaults' has no contact_name",
    ),
];

#[test]
fn silent_cases_get_a_warning() {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
    let mut missing = Vec::new();
    let object_files = WARNINGS.iter().map(|row| (row, "objects.cfg"));
    let main_file = MAIN_WARNINGS.iter().map(|row| (row, "naemon.cfg"));
    for (&(case, line, text), file) in object_files.chain(main_file) {
        let config = corpus.join(case).join("config");
        let options = Options {
            path_map: PathMap(vec![(PathBuf::from("/case"), config.clone())]),
        };
        let loaded = load(&config.join("naemon.cfg"), &options);
        let found = loaded.diags.warnings.iter().any(|warning| {
            warning.message.contains(text)
                && match &warning.loc {
                    None => line == 0,
                    Some(loc) => loc.line == line && loc.file.ends_with(file),
                }
        });
        if !found {
            let got: Vec<String> = loaded
                .diags
                .warnings
                .iter()
                .map(|w| w.to_string())
                .collect();
            missing.push(format!(
                "{case}: no warning with '{text}' at line {line}; got {got:#?}"
            ));
        }
    }
    assert!(missing.is_empty(), "{}", missing.join("\n"));
}
