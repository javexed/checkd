//! One of checkd's own command lines (`checkd:...`) has its words settled
//! by what the configuration wrote. What a plugin printed stays inside
//! the word its macro stood in, whatever it holds.

use std::path::Path;

use checkd_core::cmdline::{own_words, BETWEEN};
use checkd_core::engine::{Effect, Engine};

fn engine() -> Engine {
    let objects = "define command {\n command_name ok\n command_line /bin/true\n}\n\
         define command {\n command_name tell\n command_line checkd:notify -u http://hooks.example.test/a -t \"$HOSTNAME$/$SERVICEDESC$ is $SERVICESTATE$\" -m \"$NOTIFICATIONTYPE$: $SERVICEOUTPUT$\"\n}\n\
         define command {\n command_name bare\n command_line checkd:notify -u http://hooks.example.test/a -m $SERVICEOUTPUT$\n}\n\
         define command {\n command_name agg\n command_line checkd:aggregate $ARG1$\n}\n\
         define timeperiod {\n timeperiod_name always\n alias always\n sunday 00:00-24:00\n monday 00:00-24:00\n tuesday 00:00-24:00\n \
         wednesday 00:00-24:00\n thursday 00:00-24:00\n friday 00:00-24:00\n saturday 00:00-24:00\n}\n\
         define contact {\n contact_name ann\n host_notification_period always\n service_notification_period always\n \
         host_notification_options d,u,r\n service_notification_options w,u,c,r\n host_notification_commands tell\n service_notification_commands tell,bare\n}\n\
         define host {\n host_name web\n address 192.0.2.1\n check_command ok\n max_check_attempts 1\n check_period always\n contacts ann\n}\n\
         define service {\n host_name web\n service_description disk\n check_command ok\n max_check_attempts 1\n check_period always\n \
         notification_period always\n contacts ann\n}\n\
         define service {\n host_name web\n service_description all\n check_command agg!--min 1 'web/disk' web\n max_check_attempts 1\n check_period always\n}\n";
    let dir = std::env::temp_dir().join(format!("checkd-own-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("objects.cfg"), objects).unwrap();
    std::fs::write(dir.join("naemon.cfg"), "cfg_file=objects.cfg\ncheck_external_commands=1\n").unwrap();
    let loaded = checkd_config::load(Path::new(&dir.join("naemon.cfg")), &Default::default());
    let (Some(main), Some(objects)) = (loaded.main, loaded.objects) else { panic!("{:?}", loaded.diags.errors) };
    Engine::new(objects, &main, Box::new(checkd_core::timeperiod::Fixed(0)), 1_800_000_000_000).0
}

/// The notification command lines the engine starts when the service gets this output.
fn lines_for(output: &str) -> Vec<Vec<String>> {
    let mut engine = engine();
    let mut effects = Vec::new();
    engine.command(&format!("[1800000000] PROCESS_SERVICE_CHECK_RESULT;web;disk;2;{output}"), 1_800_000_000_000, &mut effects);
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Run(job) if job.line.starts_with("checkd:notify") => Some(own_words(&job.line)),
            _ => None,
        })
        .collect()
}

#[test]
fn output_with_quotes_and_options_is_one_word() {
    for output in [
        "DISK CRITICAL - 3.5\" bay failed",
        "x\" -u \"http://198.51.100.9/elsewhere",
        "it's 'odd' -f slack",
        "plain words -u http://198.51.100.9/",
    ] {
        let lines = lines_for(output);
        assert_eq!(lines.len(), 2, "{output}");
        // The quoted template: seven words, the last the whole message.
        let quoted = lines.iter().find(|words| words.contains(&"-t".to_string())).unwrap();
        assert_eq!(quoted.len(), 7, "{quoted:?}");
        assert_eq!(quoted[2], "http://hooks.example.test/a");
        assert_eq!(quoted[4], "web/disk is CRITICAL");
        assert_eq!(quoted[6], format!("PROBLEM: {output}"));
        assert_eq!(quoted.iter().filter(|word| *word == "-u").count(), 1);
        // And where the template did not even quote the macro.
        let bare = lines.iter().find(|words| !words.contains(&"-t".to_string())).unwrap();
        assert_eq!(bare, &["checkd:notify", "-u", "http://hooks.example.test/a", "-m", output]);
    }
}

#[test]
fn what_the_configuration_wrote_still_makes_the_words() {
    // An argument of the check command is the configuration's: its words
    // and quotes count.
    let engine = engine();
    let line = engine.check_line(checkd_core::engine::Obj::Service(0), 1_800_000_000_000).unwrap();
    let line2 = engine.check_line(checkd_core::engine::Obj::Service(1), 1_800_000_000_000).unwrap();
    let aggregate = if line.starts_with("checkd:aggregate") { line } else { line2 };
    assert_eq!(own_words(&aggregate), ["checkd:aggregate", "--min", "1", "web/disk", "web"]);
    assert!(aggregate.contains(BETWEEN));
}

#[test]
fn nothing_can_bring_the_mark_between_words() {
    let lines = lines_for(&format!("a{BETWEEN}-u{BETWEEN}http://198.51.100.9/"));
    for words in lines {
        assert_eq!(words.iter().filter(|word| *word == "-u").count(), 1, "{words:?}");
    }
}

#[test]
fn a_notification_command_that_fails_is_in_the_log() {
    use checkd_core::engine::{JobKind, JobResult, Outcome};
    let mut engine = engine();
    let mut effects = Vec::new();
    engine.command("[1800000000] PROCESS_SERVICE_CHECK_RESULT;web;disk;2;full", 1_800_000_000_000, &mut effects);
    let jobs: Vec<u64> = effects.iter().filter_map(|e| match e { Effect::Run(job) if job.kind == JobKind::Notification => Some(job.id), _ => None }).collect();
    assert_eq!(jobs.len(), 2);
    let result = |outcome, stderr: &str| JobResult { outcome, stdout: String::new(), stderr: stderr.to_string() };
    let mut log = Vec::new();
    engine.job_done(1_800_000_001_000, jobs[0], result(Outcome::Exited(1), "HTTP 500 from hooks.example.test\nmore"), &mut log);
    engine.job_done(1_800_000_001_000, jobs[1], result(Outcome::TimedOut, ""), &mut log);
    let lines: Vec<&str> = log.iter().filter_map(|e| match e { Effect::Log(line) => Some(line.as_str()), _ => None }).collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines.iter().any(|l| *l == "Warning: Contact 'ann' service notification command 'tell' failed with return code 1: HTTP 500 from hooks.example.test"), "{lines:?}");
    assert!(lines.iter().any(|l| l.starts_with("Warning: Contact 'ann' service notification command 'bare' timed out after ")), "{lines:?}");
    // One that worked says nothing.
    let mut quiet = Vec::new();
    engine.command("[1800000100] PROCESS_SERVICE_CHECK_RESULT;web;disk;0;fine", 1_800_000_100_000, &mut quiet);
    let ok: Vec<u64> = quiet.iter().filter_map(|e| match e { Effect::Run(job) if job.kind == JobKind::Notification => Some(job.id), _ => None }).collect();
    let mut log = Vec::new();
    for id in ok {
        engine.job_done(1_800_000_101_000, id, result(Outcome::Exited(0), ""), &mut log);
    }
    assert!(log.iter().all(|e| !matches!(e, Effect::Log(_))));
}
