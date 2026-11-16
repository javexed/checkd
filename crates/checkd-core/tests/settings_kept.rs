//! What commands change is kept over a restart: the settings of hosts and
//! services and the program's switches go into the snapshot and come back,
//! unless the configuration changed in between.

use std::path::Path;

use checkd_core::engine::Engine;
use checkd_core::snapshot::Snapshot;

const OBJECTS: &str = "
define command {
    command_name ok
    command_line /bin/true
}
define timeperiod {
    timeperiod_name always
    alias always
    monday 00:00-24:00
}
define host {
    host_name web
    address 192.0.2.1
    check_command ok
    max_check_attempts 3
    check_interval 5
    check_period always
    _OWNER ann
}
define service {
    host_name web
    service_description http
    check_command ok
    max_check_attempts 3
    check_interval 5
    retry_interval 1
    check_period always
}
";

fn engine(objects: &str, main_extra: &str) -> Engine {
    let dir = std::env::temp_dir().join(format!("checkd-settings-{}-{}", std::process::id(), objects.len() + main_extra.len()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("objects.cfg"), objects).unwrap();
    std::fs::write(dir.join("naemon.cfg"), format!("cfg_file=objects.cfg\ninterval_length=60\n{main_extra}")).unwrap();
    let loaded = checkd_config::load(Path::new(&dir.join("naemon.cfg")), &Default::default());
    let (Some(main), Some(objects)) = (loaded.main, loaded.objects) else { panic!("{:?}", loaded.diags.errors) };
    let (engine, _) = Engine::new(objects, &main, Box::new(checkd_core::timeperiod::Fixed(0)), 1_800_000_000_000);
    engine
}

fn commands(engine: &mut Engine, lines: &[&str]) {
    let mut effects = Vec::new();
    for line in lines {
        engine.command(&format!("[1800000000] {line}"), 1_800_000_000_000, &mut effects);
    }
}

fn kept(snapshot: &Snapshot) -> Snapshot {
    // As through a store: what was written comes back the same.
    snapshot.clone()
}

#[test]
fn settings_and_switches_come_back() {
    let mut first = engine(OBJECTS, "");
    commands(
        &mut first,
        &[
            "DISABLE_SVC_EVENT_HANDLER;web;http",
            "DISABLE_SVC_FLAP_DETECTION;web;http",
            "DISABLE_PASSIVE_SVC_CHECKS;web;http",
            "CHANGE_NORMAL_SVC_CHECK_INTERVAL;web;http;9",
            "CHANGE_RETRY_SVC_CHECK_INTERVAL;web;http;2",
            "CHANGE_MAX_SVC_CHECK_ATTEMPTS;web;http;7",
            "CHANGE_CUSTOM_HOST_VAR;web;_OWNER;bob",
            "DISABLE_EVENT_HANDLERS",
            "STOP_EXECUTING_HOST_CHECKS",
        ],
    );
    let snapshot = kept(&first.snapshot());
    let service = |e: &Engine| e.item(e.service("web", "http").unwrap()).cfg.clone();
    let host = |e: &Engine| e.item(e.host("web").unwrap()).cfg.clone();
    let modified = |e: &Engine| e.item(e.service("web", "http").unwrap()).modified;

    let mut second = engine(OBJECTS, "");
    second.restore(&snapshot);
    let (s, h) = (service(&second), host(&second));
    assert!(!s.event_handler_enabled && !s.flap_detection_enabled && !s.passive_checks_enabled);
    assert_eq!((s.check_interval, s.retry_interval, s.max_check_attempts), (9.0, 2.0, 7));
    // And the checks run by them, not only show them.
    let item = second.item(second.service("web", "http").unwrap());
    assert_eq!(item.intervals(), (9 * 60_000, 2 * 60_000));
    assert_eq!(h.custom, vec![("OWNER".to_string(), "bob".to_string())]);
    assert_eq!(modified(&second), modified(&first));
    assert_ne!(modified(&second), 0);
    assert!(!second.settings().enable_event_handlers && !second.settings().execute_host_checks);
    assert!(second.settings().execute_service_checks);

    // A setting whose configuration changed meanwhile takes the new
    // configuration; the others are kept.
    let changed = OBJECTS.replace("    check_interval 5\n    retry_interval 1", "    check_interval 6\n    retry_interval 1");
    let mut third = engine(&changed, "enable_event_handlers=0\n");
    third.restore(&snapshot);
    let s = service(&third);
    assert_eq!(s.check_interval, 6.0);
    assert_eq!((s.retry_interval, s.max_check_attempts), (2.0, 7));
    assert!(!s.event_handler_enabled);
    // Configured off now, as it was set off: off either way.
    assert!(!third.settings().enable_event_handlers);

    // Nothing changed by command: nothing is kept beyond the configuration.
    let untouched = engine(OBJECTS, "").snapshot();
    assert!(untouched.program.is_empty());
    assert!(untouched.objects.iter().all(|o| o.settings.is_empty()));
}

#[test]
fn a_soft_change_of_state_is_counted_though_not_logged() {
    let mut engine = engine(OBJECTS, "log_service_retries=0\n");
    let before = engine.state_changes();
    let mut effects = Vec::new();
    engine.command("[1800000000] PROCESS_SERVICE_CHECK_RESULT;web;http;2;broken", 1_800_000_000_000, &mut effects);
    let item = engine.item(engine.service("web", "http").unwrap());
    assert!(!item.hard && item.state == 2, "a soft CRITICAL");
    assert!(effects.iter().all(|e| !matches!(e, checkd_core::engine::Effect::Log(line) if line.starts_with("SERVICE ALERT"))));
    assert_eq!(engine.state_changes(), before + 1);
}
