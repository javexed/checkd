//! The objects of a loaded configuration, with templates applied, groups
//! expanded and defaults filled in.

use std::collections::{BTreeMap, BTreeSet};

use crate::timeperiod::{Exception, TimeRange};
use crate::values::Opts;

/// A custom variable. The name is in upper case, without the underscore.
pub type CustomVar = (String, String);

#[derive(Clone, Debug)]
pub struct Timeperiod {
    pub name: String,
    pub alias: String,
    pub exclude: Vec<String>,
    /// Sunday first.
    pub weekdays: [Vec<TimeRange>; 7],
    pub exceptions: Vec<Exception>,
}

#[derive(Clone, Debug)]
pub struct Command {
    pub name: String,
    pub line: String,
}

#[derive(Clone, Debug)]
pub struct Group<Member: Ord> {
    pub name: String,
    pub alias: String,
    /// Every member, those that come through member groups included.
    pub members: BTreeSet<Member>,
    pub notes: Option<String>,
    pub notes_url: Option<String>,
    pub action_url: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Contact {
    pub name: String,
    pub alias: String,
    pub service_notification_period: Option<String>,
    pub host_notification_period: Option<String>,
    pub service_notification_options: Opts,
    pub host_notification_options: Opts,
    pub service_notification_commands: Vec<String>,
    pub host_notification_commands: Vec<String>,
    pub email: Option<String>,
    pub pager: Option<String>,
    pub addresses: [Option<String>; 6],
    pub minimum_value: u32,
    pub host_notifications_enabled: bool,
    pub service_notifications_enabled: bool,
    pub can_submit_commands: bool,
    pub retain_status_information: bool,
    pub retain_nonstatus_information: bool,
    pub custom: Vec<CustomVar>,
}

/// What hosts and services have in common.
#[derive(Clone, Debug)]
pub struct Checkable {
    pub display_name: Option<String>,
    pub check_period: Option<String>,
    /// The command name with its arguments, as written: `name!arg1!arg2`.
    pub check_command: Option<String>,
    pub event_handler: Option<String>,
    pub contacts: Vec<String>,
    pub contact_groups: Vec<String>,
    pub notification_period: Option<String>,
    /// `o`, `d`, `u` for a host; `o`, `w`, `u`, `c` for a service.
    pub initial_state: char,
    pub check_timeout: u32,
    pub hourly_value: u32,
    pub check_interval: f64,
    pub retry_interval: f64,
    pub max_check_attempts: u32,
    pub active_checks_enabled: bool,
    pub passive_checks_enabled: bool,
    pub obsess: bool,
    pub event_handler_enabled: bool,
    pub low_flap_threshold: f64,
    pub high_flap_threshold: f64,
    pub flap_detection_enabled: bool,
    pub flap_detection_options: Opts,
    pub freshness_threshold: u32,
    pub check_freshness: bool,
    pub notification_options: Opts,
    pub notifications_enabled: bool,
    pub notification_interval: f64,
    pub first_notification_delay: f64,
    pub stalking_options: Opts,
    pub process_perf_data: bool,
    pub icon_image: Option<String>,
    pub icon_image_alt: Option<String>,
    pub notes: Option<String>,
    pub notes_url: Option<String>,
    pub action_url: Option<String>,
    pub retain_status_information: bool,
    pub retain_nonstatus_information: bool,
    pub custom: Vec<CustomVar>,
}

#[derive(Clone, Debug)]
pub struct Host {
    pub name: String,
    pub alias: String,
    pub address: String,
    pub parents: Vec<String>,
    pub check: Checkable,
    pub vrml_image: Option<String>,
    pub statusmap_image: Option<String>,
    pub coords_2d: Option<(i32, i32)>,
    pub coords_3d: Option<(f64, f64, f64)>,
}

/// Host name and service description.
pub type ServiceId = (String, String);

#[derive(Clone, Debug)]
pub struct Service {
    pub host: String,
    pub description: String,
    pub parents: Vec<ServiceId>,
    pub is_volatile: bool,
    pub check: Checkable,
}

/// `Id` is a host name or a [`ServiceId`].
#[derive(Clone, Debug)]
pub struct Dependency<Id> {
    /// What is depended on.
    pub master: Id,
    pub dependent: Id,
    pub period: Option<String>,
    pub inherits_parent: bool,
    /// States of the master in which the dependent is not checked.
    pub execution_failure: Opts,
    /// States of the master in which the dependent does not notify.
    pub notification_failure: Opts,
}

#[derive(Clone, Debug)]
pub struct Escalation<Id> {
    pub object: Id,
    pub first_notification: Option<u32>,
    pub last_notification: Option<u32>,
    pub notification_interval: f64,
    pub period: Option<String>,
    pub options: Opts,
    pub contacts: Vec<String>,
    pub contact_groups: Vec<String>,
}

/// Every object of a configuration. Maps are keyed by name.
#[derive(Clone, Debug, Default)]
pub struct Objects {
    pub timeperiods: BTreeMap<String, Timeperiod>,
    pub commands: BTreeMap<String, Command>,
    pub contactgroups: BTreeMap<String, Group<String>>,
    pub hostgroups: BTreeMap<String, Group<String>>,
    pub servicegroups: BTreeMap<String, Group<ServiceId>>,
    pub contacts: BTreeMap<String, Contact>,
    pub hosts: BTreeMap<String, Host>,
    pub services: BTreeMap<ServiceId, Service>,
    pub service_dependencies: Vec<Dependency<ServiceId>>,
    pub service_escalations: Vec<Escalation<ServiceId>>,
    pub host_dependencies: Vec<Dependency<String>>,
    pub host_escalations: Vec<Escalation<String>>,
}
