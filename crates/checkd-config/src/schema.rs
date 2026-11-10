//! Which settings each object type has, and what kind of value each takes.

use crate::raw::ObjType;

/// The kind of value a setting takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Any text. `null` cancels an inherited value.
    Text,
    /// A comma-separated list of names. `null` cancels an inherited value,
    /// and a leading `+` adds to it.
    List,
    /// 0 or 1.
    Flag,
    /// A whole number, zero or more.
    Int,
    /// A number, zero or more.
    Float,
    /// Option letters from the given set, or `a` for all, or `n` for none.
    Opts(&'static str),
    /// `x,y` as whole numbers.
    Coords2,
    /// `x,y,z` as numbers.
    Coords3,
}

pub struct Field {
    pub key: &'static str,
    pub kind: Kind,
}

const fn f(key: &'static str, kind: Kind) -> Field {
    Field { key, kind }
}

use Kind::*;

/// Option letters, each set in the order the dump writes them.
pub const HOST_NOTIFY: &str = "rdufs";
pub const SERVICE_NOTIFY: &str = "rwucfs";
pub const HOST_STATES: &str = "odu";
pub const SERVICE_STATES: &str = "owuc";
pub const HOST_FAILURE: &str = "odup";
pub const SERVICE_FAILURE: &str = "owucp";
pub const HOST_ESCALATION: &str = "rdu";
pub const SERVICE_ESCALATION: &str = "rwuc";

const TEMPLATE: [Field; 3] = [f("name", Text), f("use", Text), f("register", Flag)];

const HOST: &[Field] = &[
    f("host_name", Text),
    f("alias", Text),
    f("display_name", Text),
    f("address", Text),
    f("parents", List),
    f("hourly_value", Int),
    f("hostgroups", List),
    f("check_command", Text),
    f("check_timeout", Int),
    f("initial_state", Opts(HOST_STATES)),
    f("max_check_attempts", Int),
    f("check_interval", Float),
    f("retry_interval", Float),
    f("active_checks_enabled", Flag),
    f("passive_checks_enabled", Flag),
    f("check_period", Text),
    f("obsess", Flag),
    f("check_freshness", Flag),
    f("freshness_threshold", Int),
    f("event_handler", Text),
    f("event_handler_enabled", Flag),
    f("low_flap_threshold", Float),
    f("high_flap_threshold", Float),
    f("flap_detection_enabled", Flag),
    f("flap_detection_options", Opts(HOST_STATES)),
    f("process_perf_data", Flag),
    f("retain_status_information", Flag),
    f("retain_nonstatus_information", Flag),
    f("contacts", List),
    f("contact_groups", List),
    f("notification_interval", Float),
    f("first_notification_delay", Float),
    f("notification_period", Text),
    f("notification_options", Opts(HOST_NOTIFY)),
    f("notifications_enabled", Flag),
    f("stalking_options", Opts(HOST_STATES)),
    f("notes", Text),
    f("notes_url", Text),
    f("action_url", Text),
    f("icon_image", Text),
    f("icon_image_alt", Text),
    f("vrml_image", Text),
    f("statusmap_image", Text),
    f("2d_coords", Coords2),
    f("3d_coords", Coords3),
];

const SERVICE: &[Field] = &[
    f("host_name", List),
    f("hostgroup_name", List),
    f("service_description", Text),
    f("display_name", Text),
    f("parents", List),
    f("hourly_value", Int),
    f("servicegroups", List),
    f("is_volatile", Flag),
    f("check_command", Text),
    f("check_timeout", Int),
    f("initial_state", Opts(SERVICE_STATES)),
    f("max_check_attempts", Int),
    f("check_interval", Float),
    f("retry_interval", Float),
    f("active_checks_enabled", Flag),
    f("passive_checks_enabled", Flag),
    f("check_period", Text),
    f("obsess", Flag),
    f("check_freshness", Flag),
    f("freshness_threshold", Int),
    f("event_handler", Text),
    f("event_handler_enabled", Flag),
    f("low_flap_threshold", Float),
    f("high_flap_threshold", Float),
    f("flap_detection_enabled", Flag),
    f("flap_detection_options", Opts(SERVICE_STATES)),
    f("process_perf_data", Flag),
    f("retain_status_information", Flag),
    f("retain_nonstatus_information", Flag),
    f("notification_interval", Float),
    f("first_notification_delay", Float),
    f("notification_period", Text),
    f("notification_options", Opts(SERVICE_NOTIFY)),
    f("notifications_enabled", Flag),
    f("contacts", List),
    f("contact_groups", List),
    f("stalking_options", Opts(SERVICE_STATES)),
    f("notes", Text),
    f("notes_url", Text),
    f("action_url", Text),
    f("icon_image", Text),
    f("icon_image_alt", Text),
];

const CONTACT: &[Field] = &[
    f("contact_name", Text),
    f("alias", Text),
    f("contactgroups", List),
    f("minimum_value", Int),
    f("host_notifications_enabled", Flag),
    f("service_notifications_enabled", Flag),
    f("host_notification_period", Text),
    f("service_notification_period", Text),
    f("host_notification_options", Opts(HOST_NOTIFY)),
    f("service_notification_options", Opts(SERVICE_NOTIFY)),
    f("host_notification_commands", List),
    f("service_notification_commands", List),
    f("email", Text),
    f("pager", Text),
    f("address1", Text),
    f("address2", Text),
    f("address3", Text),
    f("address4", Text),
    f("address5", Text),
    f("address6", Text),
    f("can_submit_commands", Flag),
    f("retain_status_information", Flag),
    f("retain_nonstatus_information", Flag),
];

const COMMAND: &[Field] = &[f("command_name", Text), f("command_line", Text)];

const TIMEPERIOD: &[Field] = &[
    f("timeperiod_name", Text),
    f("alias", Text),
    f("exclude", List),
];

const HOSTGROUP: &[Field] = &[
    f("hostgroup_name", Text),
    f("alias", Text),
    f("members", List),
    f("hostgroup_members", List),
    f("notes", Text),
    f("notes_url", Text),
    f("action_url", Text),
];

const SERVICEGROUP: &[Field] = &[
    f("servicegroup_name", Text),
    f("alias", Text),
    f("members", List),
    f("servicegroup_members", List),
    f("notes", Text),
    f("notes_url", Text),
    f("action_url", Text),
];

const CONTACTGROUP: &[Field] = &[
    f("contactgroup_name", Text),
    f("alias", Text),
    f("members", List),
    f("contactgroup_members", List),
];

const HOSTDEPENDENCY: &[Field] = &[
    f("dependent_host_name", List),
    f("dependent_hostgroup_name", List),
    f("host_name", List),
    f("hostgroup_name", List),
    f("inherits_parent", Flag),
    f("execution_failure_options", Opts(HOST_FAILURE)),
    f("notification_failure_options", Opts(HOST_FAILURE)),
    f("dependency_period", Text),
];

const SERVICEDEPENDENCY: &[Field] = &[
    f("dependent_host_name", List),
    f("dependent_hostgroup_name", List),
    f("dependent_servicegroup_name", List),
    f("dependent_service_description", List),
    f("host_name", List),
    f("hostgroup_name", List),
    f("servicegroup_name", List),
    f("service_description", List),
    f("inherits_parent", Flag),
    f("execution_failure_options", Opts(SERVICE_FAILURE)),
    f("notification_failure_options", Opts(SERVICE_FAILURE)),
    f("dependency_period", Text),
];

const HOSTESCALATION: &[Field] = &[
    f("host_name", List),
    f("hostgroup_name", List),
    f("contacts", List),
    f("contact_groups", List),
    f("first_notification", Int),
    f("last_notification", Int),
    f("notification_interval", Float),
    f("escalation_period", Text),
    f("escalation_options", Opts(HOST_ESCALATION)),
];

const SERVICEESCALATION: &[Field] = &[
    f("host_name", List),
    f("hostgroup_name", List),
    f("servicegroup_name", List),
    f("service_description", List),
    f("contacts", List),
    f("contact_groups", List),
    f("first_notification", Int),
    f("last_notification", Int),
    f("notification_interval", Float),
    f("escalation_period", Text),
    f("escalation_options", Opts(SERVICE_ESCALATION)),
];

fn fields(kind: ObjType) -> &'static [Field] {
    match kind {
        ObjType::Timeperiod => TIMEPERIOD,
        ObjType::Command => COMMAND,
        ObjType::ContactGroup => CONTACTGROUP,
        ObjType::HostGroup => HOSTGROUP,
        ObjType::ServiceGroup => SERVICEGROUP,
        ObjType::Contact => CONTACT,
        ObjType::Host => HOST,
        ObjType::Service => SERVICE,
        ObjType::ServiceDependency => SERVICEDEPENDENCY,
        ObjType::ServiceEscalation => SERVICEESCALATION,
        ObjType::HostDependency => HOSTDEPENDENCY,
        ObjType::HostEscalation => HOSTESCALATION,
    }
}

/// Older or alternative names of settings, and the name used here.
pub fn canonical_key(kind: ObjType, key: &str) -> &str {
    match (kind, key) {
        (ObjType::Host | ObjType::Service, "normal_check_interval") => "check_interval",
        (ObjType::Host | ObjType::Service, "retry_check_interval") => "retry_interval",
        (ObjType::Host, "obsess_over_host") => "obsess",
        (ObjType::Service, "obsess_over_service") => "obsess",
        (ObjType::HostDependency | ObjType::ServiceDependency, "execution_failure_criteria") => {
            "execution_failure_options"
        }
        (ObjType::HostDependency | ObjType::ServiceDependency, "notification_failure_criteria") => {
            "notification_failure_options"
        }
        _ => key,
    }
}

/// The kind of a setting, or `None` when the object type has no such
/// setting. Custom variables and timeperiod day lines are not looked up
/// here.
pub fn kind_of(kind: ObjType, key: &str) -> Option<Kind> {
    TEMPLATE
        .iter()
        .chain(fields(kind))
        .find(|field| field.key == key)
        .map(|field| field.kind)
}

/// Whether a leading `+` on this setting adds to the inherited value.
pub fn is_list(kind: ObjType, key: &str) -> bool {
    kind_of(kind, key) == Some(Kind::List)
}
