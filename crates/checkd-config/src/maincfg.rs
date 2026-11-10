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
    "illegal_macro_output_chars",
    "illegal_object_name_chars",
    "interval_length",
    "lock_file",
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
    "retained_contact_host_attribute_mask",
    "retained_contact_service_attribute_mask",
    "retained_host_attribute_mask",
    "retained_process_host_attribute_mask",
    "retained_process_service_attribute_mask",
    "retained_scheduling_randomize_window",
    "retained_service_attribute_mask",
    "retain_state_information",
    "retention_update_interval",
    "service_check_timeout",
    "service_freshness_check_interval",
    "service_inter_check_delay_method",
    "service_interleave_factor",
    "service_perfdata_command",
    "service_perfdata_file",
    "service_perfdata_file_mode",
    "service_perfdata_file_processing_command",
    "service_perfdata_file_processing_interval",
    "service_perfdata_file_template",
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
/// warning.
pub fn read_text(path: &Path, diags: &mut Diagnostics) -> Option<String> {
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
            diags.error(None, format!("cannot read {}: {err}", path.display()));
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

fn read_resource_file(path: &Path, macros: &mut BTreeMap<u32, String>, diags: &mut Diagnostics) {
    let Some(text) = read_text(path, diags) else {
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

/// Reads the main configuration file and works out which object files it
/// names. The object files are not read here.
pub fn read(path: &Path, map: &PathMap, diags: &mut Diagnostics) -> Option<MainConfig> {
    let text = read_text(path, diags)?;
    let file = Arc::new(path.to_path_buf());
    let base = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let locate = |value: &str| {
        let written = Path::new(value);
        if written.is_absolute() {
            map.apply(written)
        } else {
            base.join(written)
        }
    };
    let mut main = MainConfig {
        path: path.to_path_buf(),
        ..MainConfig::default()
    };

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
                let found = locate(value);
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
                let found = locate(value);
                if found.is_dir() {
                    collect_dir(&found, &mut main.object_files, diags);
                } else {
                    diags.error(
                        Some(&loc),
                        format!("object directory {} cannot be read", found.display()),
                    );
                }
            }
            "resource_file" => read_resource_file(&locate(value), &mut main.user_macros, diags),
            "include_file" | "include_dir" => {
                diags.error(Some(&loc), format!("'{name}' is not supported yet"));
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
            _ if DEPRECATED.contains(&name) => {
                diags.warn(Some(&loc), format!("'{name}' is deprecated in Naemon"));
            }
            _ => {}
        }
        main.settings.push((name.to_string(), value.to_string()));
    }
    Some(main)
}
