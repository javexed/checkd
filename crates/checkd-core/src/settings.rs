//! The main-config settings the engine goes by, with the reference's
//! defaults for those that are not written.
//!
//! The defaults were read off the reference by leaving the setting out of
//! a config written for the purpose: retries of a check are not logged,
//! event handlers are, initial states are not, flap detection is off, a
//! service check may take 60 seconds and a host check 30, and a result
//! younger than 15 seconds is used again instead of checking again.

use checkd_config::maincfg::MainConfig;

/// How `$DATE$` and the other date macros are written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateFormat {
    /// `MM-DD-YYYY HH:MM:SS`
    Us,
    /// `DD-MM-YYYY HH:MM:SS`
    Euro,
    /// `YYYY-MM-DD HH:MM:SS`
    Iso8601,
    /// `YYYY-MM-DDTHH:MM:SS`
    StrictIso8601,
}

#[derive(Clone, Debug)]
pub struct Settings {
    /// How many seconds one unit of `check_interval` and the like is.
    pub interval_length: f64,
    pub execute_service_checks: bool,
    pub execute_host_checks: bool,
    pub enable_event_handlers: bool,
    pub enable_flap_detection: bool,
    pub low_service_flap_threshold: f64,
    pub high_service_flap_threshold: f64,
    pub low_host_flap_threshold: f64,
    pub high_host_flap_threshold: f64,
    /// Seconds.
    pub service_check_timeout: u32,
    pub host_check_timeout: u32,
    pub event_handler_timeout: u32,
    /// The state of a service whose check timed out: 0 to 3.
    pub service_check_timeout_state: u8,
    /// Seconds. 0 turns the reuse of results off.
    pub cached_host_check_horizon: i64,
    pub cached_service_check_horizon: i64,
    pub log_service_retries: bool,
    pub log_host_retries: bool,
    pub log_event_handlers: bool,
    pub log_initial_states: bool,
    pub soft_state_dependencies: bool,
    pub global_host_event_handler: Option<String>,
    pub global_service_event_handler: Option<String>,
    pub date_format: DateFormat,
    pub illegal_macro_output_chars: String,
    pub admin_email: String,
    pub admin_pager: String,
    pub enable_notifications: bool,
    pub log_notifications: bool,
    /// Seconds a notification command may run.
    pub notification_timeout: u32,
    pub check_external_commands: bool,
    pub log_external_commands: bool,
    /// Switches the engine does not act on yet, kept to be shown.
    pub process_performance_data: bool,
    pub check_service_freshness: bool,
    pub check_host_freshness: bool,
    /// Seconds between two looks at the freshness of services, of hosts.
    pub service_freshness_check_interval: u32,
    pub host_freshness_check_interval: u32,
    /// Seconds added to a freshness threshold the engine works out itself.
    pub additional_freshness_latency: u32,
    pub obsess_over_services: bool,
    pub obsess_over_hosts: bool,
    pub accept_passive_service_checks: bool,
    pub accept_passive_host_checks: bool,
    pub passive_host_checks_are_soft: bool,
    pub log_passive_checks: bool,
    /// Seconds between reads of the check result directory.
    pub check_result_reaper_frequency: u32,
    pub check_result_path: Option<String>,
    /// Host performance data: then service performance data.
    pub perfdata: [Perfdata; 2],
}

/// Where the performance data of hosts or of services goes.
#[derive(Clone, Debug, Default)]
pub struct Perfdata {
    pub file: Option<String>,
    /// With `\t` already a tab.
    pub template: String,
    /// `a` append, `w` write anew at start, `p` a pipe.
    pub mode: char,
    /// Seconds between runs of the processing command; 0 for never.
    pub processing_interval: u32,
    pub processing_command: Option<String>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            interval_length: 60.0,
            execute_service_checks: true,
            execute_host_checks: true,
            enable_event_handlers: true,
            enable_flap_detection: false,
            low_service_flap_threshold: 20.0,
            high_service_flap_threshold: 30.0,
            low_host_flap_threshold: 20.0,
            high_host_flap_threshold: 30.0,
            service_check_timeout: 60,
            host_check_timeout: 30,
            event_handler_timeout: 30,
            service_check_timeout_state: 2,
            cached_host_check_horizon: 15,
            cached_service_check_horizon: 15,
            log_service_retries: false,
            log_host_retries: false,
            log_event_handlers: true,
            log_initial_states: false,
            soft_state_dependencies: false,
            global_host_event_handler: None,
            global_service_event_handler: None,
            date_format: DateFormat::Us,
            illegal_macro_output_chars: String::new(),
            admin_email: String::new(),
            admin_pager: String::new(),
            enable_notifications: true,
            log_notifications: true,
            notification_timeout: 30,
            check_external_commands: true,
            log_external_commands: true,
            process_performance_data: false,
            check_service_freshness: true,
            check_host_freshness: false,
            service_freshness_check_interval: 60,
            host_freshness_check_interval: 60,
            additional_freshness_latency: 15,
            obsess_over_services: false,
            obsess_over_hosts: false,
            accept_passive_service_checks: true,
            accept_passive_host_checks: true,
            passive_host_checks_are_soft: false,
            log_passive_checks: true,
            check_result_reaper_frequency: 10,
            check_result_path: None,
            perfdata: [Perfdata::default(), Perfdata::default()],
        }
    }
}

/// Settings that change what is checked or logged and that this engine
/// does not act on yet. A config that sets one away from the value given
/// here gets a warning at start.
const NOT_YET: &[(&str, &str)] = &[
    ("use_timezone", ""),
    ("max_concurrent_checks", "0"),
    ("host_down_disable_service_checks", "0"),
    ("use_aggressive_host_checking", "0"),
    ("obsess_over_services", "0"),
    ("obsess_over_hosts", "0"),
    ("enable_environment_macros", "0"),
    ("translate_passive_host_checks", "0"),
];

impl Settings {
    /// Reads the settings from a main config. The second part of the
    /// answer is warnings for the person who starts the engine.
    pub fn from_main(main: &MainConfig) -> (Settings, Vec<String>) {
        let mut s = Settings::default();
        let flag = |name: &str, now: bool| match main.get(name) {
            Some(value) => number(value) != 0.0,
            None => now,
        };
        let num = |name: &str, now: f64| main.get(name).map(number).unwrap_or(now);
        let text = |name: &str| main.get(name).map(str::to_string);

        s.interval_length = num("interval_length", s.interval_length);
        s.execute_service_checks = flag("execute_service_checks", s.execute_service_checks);
        s.execute_host_checks = flag("execute_host_checks", s.execute_host_checks);
        s.enable_event_handlers = flag("enable_event_handlers", s.enable_event_handlers);
        s.enable_flap_detection = flag("enable_flap_detection", s.enable_flap_detection);
        s.low_service_flap_threshold =
            num("low_service_flap_threshold", s.low_service_flap_threshold);
        s.high_service_flap_threshold =
            num("high_service_flap_threshold", s.high_service_flap_threshold);
        s.low_host_flap_threshold = num("low_host_flap_threshold", s.low_host_flap_threshold);
        s.high_host_flap_threshold = num("high_host_flap_threshold", s.high_host_flap_threshold);
        s.service_check_timeout = num("service_check_timeout", 60.0) as u32;
        s.host_check_timeout = num("host_check_timeout", 30.0) as u32;
        s.event_handler_timeout = num("event_handler_timeout", 30.0) as u32;
        s.service_check_timeout_state = match main.get("service_check_timeout_state") {
            Some("o") => 0,
            Some("w") => 1,
            Some("u") => 3,
            _ => 2,
        };
        s.cached_host_check_horizon = num("cached_host_check_horizon", 15.0) as i64;
        s.cached_service_check_horizon = num("cached_service_check_horizon", 15.0) as i64;
        s.log_service_retries = flag("log_service_retries", s.log_service_retries);
        s.log_host_retries = flag("log_host_retries", s.log_host_retries);
        s.log_event_handlers = flag("log_event_handlers", s.log_event_handlers);
        s.log_initial_states = flag("log_initial_states", s.log_initial_states);
        s.soft_state_dependencies = flag("soft_state_dependencies", s.soft_state_dependencies);
        s.global_host_event_handler = text("global_host_event_handler").filter(|v| !v.is_empty());
        s.global_service_event_handler =
            text("global_service_event_handler").filter(|v| !v.is_empty());
        s.date_format = match main.get("date_format") {
            Some("euro") => DateFormat::Euro,
            Some("iso8601") => DateFormat::Iso8601,
            Some("strict-iso8601") => DateFormat::StrictIso8601,
            _ => DateFormat::Us,
        };
        s.illegal_macro_output_chars = text("illegal_macro_output_chars").unwrap_or_default();
        s.admin_email = text("admin_email").unwrap_or_default();
        s.admin_pager = text("admin_pager").unwrap_or_default();
        s.enable_notifications = flag("enable_notifications", s.enable_notifications);
        s.log_notifications = flag("log_notifications", s.log_notifications);
        s.notification_timeout = num("notification_timeout", 30.0) as u32;
        s.check_external_commands = flag("check_external_commands", s.check_external_commands);
        s.log_external_commands = flag("log_external_commands", s.log_external_commands);
        s.process_performance_data = flag("process_performance_data", s.process_performance_data);
        s.check_service_freshness = flag("check_service_freshness", s.check_service_freshness);
        s.check_host_freshness = flag("check_host_freshness", s.check_host_freshness);
        s.service_freshness_check_interval = num("service_freshness_check_interval", 60.0).max(1.0) as u32;
        s.host_freshness_check_interval = num("host_freshness_check_interval", 60.0).max(1.0) as u32;
        s.additional_freshness_latency = num("additional_freshness_latency", 15.0).max(0.0) as u32;
        s.obsess_over_services = flag("obsess_over_services", s.obsess_over_services);
        s.obsess_over_hosts = flag("obsess_over_hosts", s.obsess_over_hosts);
        s.accept_passive_service_checks = flag("accept_passive_service_checks", s.accept_passive_service_checks);
        s.accept_passive_host_checks = flag("accept_passive_host_checks", s.accept_passive_host_checks);
        s.passive_host_checks_are_soft = flag("passive_host_checks_are_soft", s.passive_host_checks_are_soft);
        s.log_passive_checks = flag("log_passive_checks", s.log_passive_checks);
        s.check_result_reaper_frequency = num("check_result_reaper_frequency", 10.0).max(1.0) as u32;
        s.check_result_path = text("check_result_path").filter(|v| !v.is_empty());
        for (place, kind, default) in [
            (0, "host", "[HOSTPERFDATA]\\t$TIMET$\\t$HOSTNAME$\\t$HOSTEXECUTIONTIME$\\t$HOSTOUTPUT$\\t$HOSTPERFDATA$"),
            (
                1,
                "service",
                "[SERVICEPERFDATA]\\t$TIMET$\\t$HOSTNAME$\\t$SERVICEDESC$\\t$SERVICEEXECUTIONTIME$\\t$SERVICELATENCY$\\t$SERVICEOUTPUT$\\t$SERVICEPERFDATA$",
            ),
        ] {
            let template = text(&format!("{kind}_perfdata_file_template")).unwrap_or_else(|| default.to_string());
            s.perfdata[place] = Perfdata {
                file: text(&format!("{kind}_perfdata_file")).filter(|v| !v.is_empty()),
                template: template.replace("\\t", "\t"),
                mode: text(&format!("{kind}_perfdata_file_mode"))
                    .and_then(|m| m.trim().chars().next())
                    .unwrap_or('a'),
                processing_interval: num(&format!("{kind}_perfdata_file_processing_interval"), 0.0).max(0.0) as u32,
                processing_command: text(&format!("{kind}_perfdata_file_processing_command")).filter(|v| !v.is_empty()),
            };
        }

        let mut warnings = Vec::new();
        for (name, quiet) in NOT_YET {
            if let Some(value) = main.get(name) {
                if value != *quiet {
                    warnings.push(format!(
                        "{name}={value} is not acted on yet: the engine goes on as if it were not set"
                    ));
                }
            }
        }
        if s.interval_length <= 0.0 {
            warnings.push("interval_length is not above 0: 60 is used".to_string());
            s.interval_length = 60.0;
        }
        (s, warnings)
    }
}

/// A number the way the reference reads one: the digits it starts with,
/// and 0 when it starts with none.
fn number(value: &str) -> f64 {
    let value = value.trim();
    let mut end = 0;
    let mut dot = false;
    for (at, c) in value.char_indices() {
        let ok = c.is_ascii_digit() || (at == 0 && (c == '-' || c == '+')) || (c == '.' && !dot);
        if !ok {
            break;
        }
        dot |= c == '.';
        end = at + c.len_utf8();
    }
    value[..end].parse().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(number("15"), 15.0);
        assert_eq!(number(" 2.5 "), 2.5);
        assert_eq!(number("3abc"), 3.0);
        assert_eq!(number("often"), 0.0);
        assert_eq!(number("-1"), -1.0);
    }

    #[test]
    fn from_a_main_config() {
        let mut main = MainConfig::default();
        for (name, value) in [
            ("interval_length", "1"),
            ("log_host_retries", "1"),
            ("cached_host_check_horizon", "0"),
            ("date_format", "iso8601"),
            ("use_timezone", "Europe/Amsterdam"),
            ("max_concurrent_checks", "0"),
        ] {
            main.settings.push((name.to_string(), value.to_string()));
        }
        let (s, warnings) = Settings::from_main(&main);
        assert_eq!(s.interval_length, 1.0);
        assert!(s.log_host_retries);
        assert!(!s.log_service_retries);
        assert_eq!(s.cached_host_check_horizon, 0);
        assert_eq!(s.cached_service_check_horizon, 15);
        assert_eq!(s.date_format, DateFormat::Iso8601);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].starts_with("use_timezone="));
    }
}
