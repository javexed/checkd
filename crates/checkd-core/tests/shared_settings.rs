//! Hosts and services with the same settings hold one copy of them, the
//! configuration and the engine alike; a command that changes a setting
//! of one changes it for that one only.

use std::path::Path;
use std::sync::Arc;

use checkd_core::engine::{Engine, Obj};

fn engine() -> Engine {
    let mut objects = String::from(
        "define command {\n command_name ok\n command_line /bin/true\n}\n\
         define timeperiod {\n timeperiod_name always\n alias always\n monday 00:00-24:00\n}\n\
         define host {\n host_name web\n address 192.0.2.1\n check_command ok\n max_check_attempts 3\n check_period always\n}\n",
    );
    for name in ["a", "b", "c"] {
        objects.push_str(&format!(
            "define service {{\n host_name web\n service_description {name}\n check_command ok\n max_check_attempts 3\n \
             check_interval 5\n retry_interval 1\n check_period always\n{}}}\n",
            if name == "c" { " notes not as the others\n" } else { "" }
        ));
    }
    let dir = std::env::temp_dir().join(format!("checkd-shared-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("objects.cfg"), objects).unwrap();
    std::fs::write(dir.join("naemon.cfg"), "cfg_file=objects.cfg\ninterval_length=60\n").unwrap();
    let loaded = checkd_config::load(Path::new(&dir.join("naemon.cfg")), &Default::default());
    let (Some(main), Some(objects)) = (loaded.main, loaded.objects) else { panic!("{:?}", loaded.diags.errors) };
    Engine::new(objects, &main, Box::new(checkd_core::timeperiod::Fixed(0)), 1_800_000_000_000).0
}

#[test]
fn the_same_settings_are_held_once() {
    let engine = engine();
    let (a, b, c) = (Obj::Service(0), Obj::Service(1), Obj::Service(2));
    assert!(Arc::ptr_eq(&engine.item(a).cfg, &engine.item(b).cfg));
    assert!(!Arc::ptr_eq(&engine.item(a).cfg, &engine.item(c).cfg));
    // The engine's are the configuration's.
    let configured = &engine.objects().services[&("web".to_string(), "a".to_string())].check;
    assert!(Arc::ptr_eq(configured, &engine.item(a).cfg));
}

#[test]
fn a_command_changes_the_settings_of_its_object_only() {
    let mut engine = engine();
    let (a, b) = (Obj::Service(0), Obj::Service(1));
    let mut effects = Vec::new();
    for line in ["DISABLE_SVC_NOTIFICATIONS;web;a", "CHANGE_MAX_SVC_CHECK_ATTEMPTS;web;a;7"] {
        engine.command(&format!("[1800000000] {line}"), 1_800_000_000_000, &mut effects);
    }
    assert!(!engine.item(a).cfg.notifications_enabled && engine.item(a).cfg.max_check_attempts == 7);
    assert!(engine.item(b).cfg.notifications_enabled && engine.item(b).cfg.max_check_attempts == 3);
    // What the configuration says is as it was.
    let configured = engine.objects().services[&("web".to_string(), "a".to_string())].check.clone();
    assert!(configured.notifications_enabled && configured.max_check_attempts == 3);
    // A switch set to what it is already takes nothing apart.
    engine.command("[1800000000] ENABLE_SVC_NOTIFICATIONS;web;b", 1_800_000_000_000, &mut effects);
    assert!(Arc::ptr_eq(&engine.item(b).cfg, &configured));
}
