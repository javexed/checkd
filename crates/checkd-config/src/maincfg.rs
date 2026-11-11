//! The main configuration file: `name=value` lines.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::diag::{Diagnostics, Loc};

/// Every setting the loader knows by name. Most are only stored for now:
/// the parts of the engine that use them do not exist yet.
const KNOWN: &[&str] = &[
    "accept_passive_host_checks",
    "accept_passive_service_checks",
    "additional_freshness_latency",
    "admin_email",
    "admin_pager",
    "allow_circular_dependencies",
    "allow_empty_hostgroup_assignment",
    "auto_reschedule_checks",
    "auto_rescheduling_interval",
    "auto_rescheduling_window",
    "broker_module",
    "cached_host_check_horizon",
    "cached_service_check_horizon",
    "cfg_dir",
    "cfg_file",
    "check_external_commands",
    "check_for_orphaned_hosts",
    "check_for_orphaned_services",
    "check_host_freshness",
    "check_result_path",
    "check_result_reaper_frequency",
    "check_service_freshness",
    "check_workers",
    "child_processes_fork_twice",
    "command_file",
    "config_rel_path",
    "date_format",
    "debug_file",
    "debug_level",
    "debug_verbosity",
    "enable_environment_macros",
    "enable_event_handlers",
    "enable_flap_detection",
    "enable_notification_suppression_reason_logging",
    "enable_notifications",
    "enable_predictive_host_dependency_checks",
    "enable_predictive_service_dependency_checks",
    "event_broker_options",
    "event_handler_timeout",
    "execute_host_checks",
    "execute_service_checks",
    "free_child_process_memory",
    "global_host_event_handler",
    "global_host_notification_handler",
    "global_service_event_handler",
    "global_service_notification_handler",
    "high_host_flap_threshold",
    "high_service_flap_threshold",
    "host_check_timeout",
    "host_down_disable_service_checks",
    "host_freshness_check_interval",
    "host_inter_check_delay_method",
    "host_perfdata_command",
    "host_perfdata_file",
    "host_perfdata_file_mode",
    "host_perfdata_file_processing_command",
    "host_perfdata_file_processing_interval",
    "host_perfdata_file_template",
    "host_skip_check_dependency_status",
    "illegal_macro_output_chars",
    "illegal_object_name_chars",
    "include_dir",
    "include_file",
    "interval_length",
    "lock_file",
    "log_archive_path",
    "log_current_states",
    "log_event_handlers",
    "log_external_commands",
    "log_file",
    "log_global_notifications",
    "log_host_retries",
    "log_initial_states",
    "log_notifications",
    "log_passive_checks",
    "log_service_retries",
    "low_host_flap_threshold",
    "low_service_flap_threshold",
    "max_check_result_file_age",
    "max_check_result_reaper_time",
    "max_concurrent_checks",
    "max_debug_file_size",
    "max_host_check_spread",
    "max_service_check_spread",
    "naemon_group",
    "naemon_user",
    "notification_timeout",
    "object_cache_file",
    "obsess_over_hosts",
    "obsess_over_services",
    "ochp_command",
    "ochp_timeout",
    "ocsp_command",
    "ocsp_timeout",
    "passive_host_checks_are_soft",
    "perfdata_timeout",
    "precached_object_file",
    "process_performance_data",
    "query_socket",
    "resource_file",
    "retain_state_information",
    "retained_contact_host_attribute_mask",
    "retained_contact_service_attribute_mask",
    "retained_host_attribute_mask",
    "retained_process_host_attribute_mask",
    "retained_process_service_attribute_mask",
    "retained_scheduling_randomize_window",
    "retained_service_attribute_mask",
    "retention_update_interval",
    "service_check_timeout",
    "service_check_timeout_state",
    "service_freshness_check_interval",
    "service_inter_check_delay_method",
    "service_interleave_factor",
    "service_parents_disable_service_checks",
    "service_perfdata_command",
    "service_perfdata_file",
    "service_perfdata_file_mode",
    "service_perfdata_file_processing_command",
    "service_perfdata_file_processing_interval",
    "service_perfdata_file_template",
    "service_skip_check_dependency_status",
    "service_skip_check_host_down_status",
    "soft_state_dependencies",
    "state_retention_file",
    "status_file",
    "status_update_interval",
    "temp_file",
    "temp_path",
    "translate_passive_host_checks",
    "use_aggressive_host_checking",
    "use_large_installation_tweaks",
    "use_regexp_matching",
    "use_retained_program_state",
    "use_retained_scheduling_info",
    "use_syslog",
    "use_timezone",
    "use_true_regexp_matching",
];

/// Settings the reference version reports as deprecated.
const DEPRECATED: &[&str] = &[
    "naemon_user",
    "naemon_group",
    "service_inter_check_delay_method",
    "max_service_check_spread",
    "service_interleave_factor",
    "host_inter_check_delay_method",
    "max_host_check_spread",
    "auto_reschedule_checks",
    "auto_rescheduling_interval",
    "auto_rescheduling_window",
    "use_large_installation_tweaks",
    "free_child_process_memory",
    "child_processes_fork_twice",
    "enable_environment_macros",
];

/// The main configuration as read.
#[derive(Clone, Debug, Default)]
pub struct MainConfig {
    /// The main configuration file itself.
    pub path: PathBuf,
    /// Every setting in file order. A name can come more than once.
    pub settings: Vec<(String, String)>,
    /// Object files, in the order they are read: `cfg_file` and `cfg_dir`
    /// lines in file order, and the files of a directory sorted by name.
    pub object_files: Vec<PathBuf>,
    /// `$USERn$` macros from the resource files, by number.
    pub user_macros: BTreeMap<u32, String>,
}

impl MainConfig {
    /// The value of a setting. When it is given more than once the last
    /// one counts.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.settings
            .iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn flag(&self, name: &str) -> bool {
        self.get(name) == Some("1")
    }
}

/// Maps paths written in a configuration to where the files are now, for
/// checking a configuration away from the machine it is written for.
#[derive(Clone, Debug, Default)]
pub struct PathMap(pub Vec<(PathBuf, PathBuf)>);

impl PathMap {
    fn apply(&self, path: &Path) -> PathBuf {
        for (from, to) in &self.0 {
            if let Ok(rest) = path.strip_prefix(from) {
                return to.join(rest);
            }
        }
        path.to_path_buf()
    }
}

/// Reads a file as text. Bytes that are not UTF-8 are replaced, with a
/// warning. `from` is the line that names the file, for the error when it
/// cannot be read.
pub fn read_text(path: &Path, from: Option<&Loc>, diags: &mut Diagnostics) -> Option<String> {
    match fs::read(path) {
        Ok(bytes) => Some(String::from_utf8(bytes).unwrap_or_else(|err| {
            diags.warn(
                None,
                format!(
                    "{} is not UTF-8; the bytes that are not were replaced",
                    path.display()
                ),
            );
            String::from_utf8_lossy(err.as_bytes()).into_owned()
        })),
        Err(err) => {
            diags.error(from, format!("cannot read {}: {err}", path.display()));
            None
        }
    }
}

/// Adds the `.cfg` files of a directory and of the directories below it.
/// Names that start with a dot are skipped.
fn collect_dir(dir: &Path, out: &mut Vec<PathBuf>, diags: &mut Diagnostics) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            diags.error(
                None,
                format!("cannot read directory {}: {err}", dir.display()),
            );
            return;
        }
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    paths.sort();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect_dir(&path, out, diags);
        } else if name.ends_with(".cfg") {
            out.push(path);
        }
    }
}

fn read_resource_file(
    path: &Path,
    from: &Loc,
    macros: &mut BTreeMap<u32, String>,
    diags: &mut Diagnostics,
) {
    let Some(text) = read_text(path, Some(from), diags) else {
        return;
    };
    let file = Arc::new(path.to_path_buf());
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let loc = Loc {
            file: file.clone(),
            line: index as u32 + 1,
        };
        let number = line
            .split_once('=')
            .and_then(|(name, value)| {
                Some((name.trim().strip_prefix("$USER")?.strip_suffix('$')?, value))
            })
            .and_then(|(number, value)| Some((number.parse::<u32>().ok()?, value)));
        match number {
            Some((number, value)) => {
                macros.insert(number, value.to_string());
            }
            None => diags.error(
                Some(&loc),
                "a resource file line must be written $USERn$=value",
            ),
        }
    }
}

/// Settings that take 0 or 1 and nothing else: the reference refuses any
/// other value. Only the ones that were tried are listed.
const FLAGS: &[&str] = &[
    "enable_notification_suppression_reason_logging",
    "log_current_states",
];

/// Settings that take a state as a number from -2 to 3. The reference
/// reads the whole number the value starts with, and 0 when it starts with
/// none, and refuses a number outside that range.
const STATES: &[&str] = &[
    "host_skip_check_dependency_status",
    "service_skip_check_dependency_status",
    "service_skip_check_host_down_status",
];

/// Reads the main configuration file and works out which object files it
/// names. The object files are not read here.
pub fn read(path: &Path, map: &PathMap, diags: &mut Diagnostics) -> Option<MainConfig> {
    let mut main = MainConfig {
        path: path.to_path_buf(),
        ..MainConfig::default()
    };
    let mut reader = Reader {
        base: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
        map,
        open: Vec::new(),
    };
    reader.file(path, None, &mut main, diags).then_some(main)
}

struct Reader<'a> {
    /// The directory that a path that is not absolute is taken from: the
    /// one of the main configuration file, until a `config_rel_path` line
    /// names another. It holds for the lines that follow, also in a file
    /// that is included from another directory.
    base: PathBuf,
    map: &'a PathMap,
    /// The files being read, the main configuration file first.
    open: Vec<PathBuf>,
}

impl Reader<'_> {
    fn locate(&self, value: &str) -> PathBuf {
        let written = Path::new(value);
        if written.is_absolute() {
            self.map.apply(written)
        } else {
            self.base.join(written)
        }
    }

    /// Reads one file of `name=value` lines: the main configuration file,
    /// or a file it includes at `from`. An included file is read as if its
    /// lines stood in place of the line that includes it. Returns whether
    /// the file could be read at all.
    fn file(
        &mut self,
        path: &Path,
        from: Option<&Loc>,
        main: &mut MainConfig,
        diags: &mut Diagnostics,
    ) -> bool {
        if self.open.iter().any(|open| open == path) {
            diags.error(
                from,
                format!(
                    "{} includes itself, directly or through other files",
                    path.display()
                ),
            );
            return false;
        }
        let Some(text) = read_text(path, from, diags) else {
            return false;
        };
        self.open.push(path.to_path_buf());
        let file = Arc::new(path.to_path_buf());

        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let loc = Loc {
                file: file.clone(),
                line: index as u32 + 1,
            };
            let Some((name, value)) = line.split_once('=') else {
                diags.error(
                    Some(&loc),
                    "a main configuration line must be written name=value",
                );
                continue;
            };
            let (name, value) = (name.trim(), value.trim());
            if value.is_empty() {
                diags.error(Some(&loc), format!("'{name}' has no value"));
                continue;
            }
            match name {
                "cfg_file" => {
                    let found = self.locate(value);
                    if found.is_file() {
                        main.object_files.push(found);
                    } else {
                        diags.error(
                            Some(&loc),
                            format!("object file {} cannot be read", found.display()),
                        );
                    }
                }
                "cfg_dir" => {
                    let found = self.locate(value);
                    if found.is_dir() {
                        collect_dir(&found, &mut main.object_files, diags);
                    } else {
                        diags.error(
                            Some(&loc),
                            format!("object directory {} cannot be read", found.display()),
                        );
                    }
                }
                "resource_file" => {
                    read_resource_file(&self.locate(value), &loc, &mut main.user_macros, diags)
                }
                "config_rel_path" => {
                    let written = Path::new(value);
                    if written.is_absolute() {
                        self.base = self.map.apply(written);
                    } else {
                        diags.error(
                            Some(&loc),
                            format!(
                                "config_rel_path must be an absolute path, not '{value}'. Naemon takes such a \
                                 path from the directory it is started in"
                            ),
                        );
                    }
                }
                "include_file" => {
                    let found = self.locate(value);
                    self.file(&found, Some(&loc), main, diags);
                }
                "include_dir" => {
                    let found = self.locate(value);
                    match included_files(&found) {
                        Ok(paths) => {
                            for path in paths {
                                self.file(&path, Some(&loc), main, diags);
                            }
                        }
                        Err(err) => diags.error(
                            Some(&loc),
                            format!("cannot read directory {}: {err}", found.display()),
                        ),
                    }
                }
                "broker_module" => {
                    diags.warn(
                        Some(&loc),
                        "broker_module is ignored: checkd does not load event broker modules",
                    );
                }
                _ if !KNOWN.contains(&name) => {
                    diags.error(
                        Some(&loc),
                        format!("'{name}' is not a main configuration setting"),
                    );
                }
                _ if FLAGS.contains(&name) && value != "0" && value != "1" => {
                    diags.error(Some(&loc), format!("'{name}' takes 0 or 1, not '{value}'"));
                }
                _ if STATES.contains(&name) => match leading_number(value) {
                    Some(number) if (-2..=3).contains(&number) => {
                        if value != number.to_string() {
                            diags.warn(
                                Some(&loc),
                                format!(
                                    "'{name}' takes a number from -2 to 3; Naemon reads '{value}' as {number}, \
                                     and so does checkd"
                                ),
                            );
                        }
                    }
                    _ => diags.error(
                        Some(&loc),
                        format!("'{name}' takes a number from -2 to 3, not '{value}'"),
                    ),
                },
                "service_check_timeout_state" if !["c", "u", "w", "o"].contains(&value) => {
                    diags.error(
                        Some(&loc),
                        format!("'{name}' takes c, u, w or o, not '{value}'"),
                    );
                }
                "allow_empty_hostgroup_assignment" if value != "0" && value != "1" => {
                    let read = if leading_number_is_positive(value) {
                        "on"
                    } else {
                        "off"
                    };
                    diags.warn(
                        Some(&loc),
                        format!("'{name}' is written 0 or 1; Naemon reads '{value}' as {read}, and so does checkd"),
                    );
                }
                _ if DEPRECATED.contains(&name) => {
                    diags.warn(Some(&loc), format!("'{name}' is deprecated in Naemon"));
                }
                _ => {}
            }
            main.settings.push((name.to_string(), value.to_string()));
        }
        self.open.pop();
        true
    }
}

/// How the reference reads `allow_empty_hostgroup_assignment`: the whole
/// number the value starts with, and on when that is above zero. `1x` is
/// on; `0x`, `-1` and `abc` are off.
pub fn leading_number_is_positive(value: &str) -> bool {
    leading_number(value).is_none_or(|number| number > 0)
}

/// The whole number a value starts with, after an optional sign: 3 for
/// `3x`, 1 for `1.9`, 0 for `abc`. `None` when it is too large to hold.
fn leading_number(value: &str) -> Option<i64> {
    let value = value.trim_start();
    let (negative, rest) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value.strip_prefix('+').unwrap_or(value)),
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return Some(0);
    }
    let number: i64 = digits.parse().ok()?;
    Some(if negative { -number } else { number })
}

/// The files that `include_dir` reads: the files of the directory itself
/// whose names end in `.cfg` and do not start with a dot, sorted by name.
/// Directories below it are not looked into.
fn included_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if name.ends_with(".cfg") && !name.starts_with('.') {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}
