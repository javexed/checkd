//! Writes `src/columns.rs`: the columns checkd answers for, with their
//! types, for the `columns` table. Run by hand when columns are added:
//!
//!     CHECKD_COLUMNS_FROM=<answer of the reference to `GET columns`> \
//!         cargo test -p checkd-livestatus --test make_columns -- --nocapture
//!
//! The answer gives each column's table, name and type; a column is
//! listed when checkd has a value for it.

use std::path::Path;

use checkd_core::engine::Engine;
use checkd_core::timeperiod::Fixed;
use checkd_livestatus::{Program, Source, tables};

#[test]
fn make_columns() {
    let Ok(from) = std::env::var("CHECKD_COLUMNS_FROM") else { return };
    let text = std::fs::read_to_string(&from).unwrap();
    let body = &text[16..];
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runs/scenarios/livestatus-thruk");
    let loaded = checkd_config::load(&dir.join("naemon.cfg"), &Default::default());
    let (main, objects) = (loaded.main.unwrap(), loaded.objects.unwrap());
    let (mut engine, _) = Engine::new(objects, &main, Box::new(Fixed(0)), 1_800_000_000_000);
    let mut out = Vec::new();
    engine.start(&mut out);
    let now = 1_800_000_000_000;
    engine.command("[1800000000] ADD_SVC_COMMENT;web;disk;1;ann;a", now, &mut out);
    engine.command("[1800000000] SCHEDULE_HOST_DOWNTIME;app;1800000000;1800003600;1;0;0;ann;b", now, &mut out);
    let program = Program::default();
    let log = vec![(1800000000, "SERVICE ALERT: web;disk;OK;HARD;1;x".to_string())];
    let source = Source { engine: &engine, now, log: &log, program: &program, window: None };
    let mut lines = Vec::new();
    for row in body.split("],\n[") {
        let fields: Vec<String> = row
            .trim_matches(|c| c == '[' || c == ']' || c == '\n')
            .split("\",\"")
            .map(|s| s.trim_matches('"').to_string())
            .collect();
        let [table, name, ty] = &fields[..] else { continue };
        let Some(rows) = tables::rows(&source, table) else { continue };
        let Some(first) = rows.first() else { continue };
        if tables::get(&source, first, name).is_some() {
            lines.push(format!("    (\"{table}\", \"{name}\", \"{ty}\"),"));
        }
    }
    let file = format!(
        "//! The columns checkd answers for, with their types: what the `columns`\n\
         //! table lists. Made by `tests/make_columns.rs` from the reference's\n\
         //! list of names and types; do not edit by hand.\n\n\
         pub const COLUMNS: &[(&str, &str, &str)] = &[\n{}\n];\n",
        lines.join("\n")
    );
    std::fs::write(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/columns.rs"), file).unwrap();
    println!("{} columns", lines.len());
}
