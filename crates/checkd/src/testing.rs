//! For tests: an engine from a few objects, on a fixed clock in UTC.

use checkd_core::engine::{Engine, Ms};

/// 2027-01-15, a Friday, 08:00:00 UTC.
pub const NOW: Ms = 1_800_000_000_000;

/// The objects every test has: a command `ok` and a timeperiod `always`.
pub const BASE: &str = "define command {\n command_name ok\n command_line /bin/true\n}\n\
    define timeperiod {\n timeperiod_name always\n alias always\n sunday 00:00-24:00\n monday 00:00-24:00\n tuesday 00:00-24:00\n \
    wednesday 00:00-24:00\n thursday 00:00-24:00\n friday 00:00-24:00\n saturday 00:00-24:00\n}\n";

pub fn engine(objects: &str, main_extra: &str) -> Engine {
    let dir = std::env::temp_dir().join(format!(
        "checkd-test-{}-{:?}-{}",
        std::process::id(),
        std::thread::current().id(),
        objects.len() + main_extra.len()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("objects.cfg"), format!("{BASE}{objects}")).unwrap();
    std::fs::write(dir.join("naemon.cfg"), format!("cfg_file=objects.cfg\ncheck_external_commands=1\n{main_extra}")).unwrap();
    let loaded = checkd_config::load(&dir.join("naemon.cfg"), &Default::default());
    let (Some(main), Some(objects)) = (loaded.main, loaded.objects) else { panic!("{:?}", loaded.diags.errors) };
    Engine::new(objects, &main, Box::new(checkd_core::timeperiod::Fixed(0)), NOW).0
}

/// Runs external commands, as at `now`.
pub fn commands(engine: &mut Engine, now: Ms, lines: &[String]) {
    let mut effects = Vec::new();
    for line in lines {
        engine.command(line, now, &mut effects);
    }
}
