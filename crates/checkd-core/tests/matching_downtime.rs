//! SCHEDULE_MATCHING_DOWNTIME: one command, a fixed downtime for every
//! host, or every service, whose names fit a pattern, or the members of a
//! host group.

use std::path::Path;

use checkd_core::engine::{Effect, Engine, Obj};

const NOW: i64 = 1_800_000_000_000;

fn engine() -> Engine {
    let mut objects = String::from(
        "define command {\n command_name ok\n command_line /bin/true\n}\n\
         define timeperiod {\n timeperiod_name always\n alias always\n friday 00:00-24:00\n}\n\
         define hostgroup {\n hostgroup_name dbs\n alias dbs\n}\n",
    );
    for (host, group) in [("web1", ""), ("web2", ""), ("eu-db-1", "dbs"), ("us-db-1", "dbs"), ("mail", "")] {
        let groups = if group.is_empty() { String::new() } else { format!(" hostgroups {group}\n") };
        objects.push_str(&format!(
            "define host {{\n host_name {host}\n address 192.0.2.1\n check_command ok\n max_check_attempts 1\n check_period always\n{groups}}}\n"
        ));
        for service in ["backup", "disk"] {
            objects.push_str(&format!(
                "define service {{\n host_name {host}\n service_description {service}\n check_command ok\n max_check_attempts 1\n check_period always\n}}\n"
            ));
        }
    }
    let dir = std::env::temp_dir().join(format!("checkd-matching-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("objects.cfg"), objects).unwrap();
    std::fs::write(dir.join("naemon.cfg"), "cfg_file=objects.cfg\ncheck_external_commands=1\n").unwrap();
    let loaded = checkd_config::load(Path::new(&dir.join("naemon.cfg")), &Default::default());
    let (Some(main), Some(objects)) = (loaded.main, loaded.objects) else { panic!("{:?}", loaded.diags.errors) };
    Engine::new(objects, &main, Box::new(checkd_core::timeperiod::Fixed(0)), NOW).0
}

/// What got a downtime from one command, by name, sorted.
fn downtimes_for(hosts: &str, services: &str) -> Vec<String> {
    let mut engine = engine();
    let mut effects = Vec::new();
    let (start, end) = (NOW / 1000, NOW / 1000 + 3600);
    engine.command(&format!("[{start}] SCHEDULE_MATCHING_DOWNTIME;{hosts};{services};{start};{end};ann;patching"), NOW, &mut effects);
    let mut names: Vec<String> = engine
        .downtimes()
        .map(|d| {
            assert!(d.fixed && d.start == start && d.end == end && d.author == "ann" && d.comment == "patching");
            match d.obj {
                Obj::Host(_) => engine.log_name(d.obj),
                Obj::Service(_) => engine.log_name(d.obj).replace(';', "/"),
            }
        })
        .collect();
    names.sort();
    names
}

#[test]
fn hosts_that_fit_get_a_downtime() {
    assert_eq!(downtimes_for("web*", ""), ["web1", "web2"]);
    assert_eq!(downtimes_for("*-db-?", ""), ["eu-db-1", "us-db-1"]);
    assert_eq!(downtimes_for("@dbs", ""), ["eu-db-1", "us-db-1"]);
    assert_eq!(downtimes_for("*", "").len(), 5);
}

#[test]
fn with_services_the_services_get_it_and_not_their_hosts() {
    assert_eq!(downtimes_for("*", "backup"), ["eu-db-1/backup", "mail/backup", "us-db-1/backup", "web1/backup", "web2/backup"]);
    assert_eq!(downtimes_for("@dbs", "d*"), ["eu-db-1/disk", "us-db-1/disk"]);
    assert_eq!(downtimes_for("web1", "*"), ["web1/backup", "web1/disk"]);
}

#[test]
fn what_fits_nothing_makes_nothing() {
    assert!(downtimes_for("nowhere*", "").is_empty());
    assert!(downtimes_for("@nogroup", "").is_empty());
    assert!(downtimes_for("web*", "nothing").is_empty());
}

#[test]
fn it_is_in_the_log_as_downtimes_starting() {
    let mut engine = engine();
    let mut effects = Vec::new();
    let start = NOW / 1000;
    engine.command(&format!("[{start}] SCHEDULE_MATCHING_DOWNTIME;web*;;{start};{};ann;patching", start + 3600), NOW, &mut effects);
    engine.advance(NOW + 1000, &mut effects);
    let started = effects.iter().filter(|e| matches!(e, Effect::Log(line) if line.contains("DOWNTIME ALERT") && line.contains("STARTED"))).count();
    assert_eq!(started, 2, "{effects:?}");
}
