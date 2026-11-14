//! Whether the engine holds on to memory check by check: a million checks
//! with results given at once, no processes. Run by hand:
//! `cargo test --release -p checkd-core --test memory -- --ignored --nocapture`

use std::path::Path;

use checkd_core::engine::{Effect, Engine, JobResult, Outcome};

/// Counts what is allocated and not yet freed: what the program holds,
/// apart from what the allocator keeps around it.
struct Counting;
static LIVE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
unsafe impl std::alloc::GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        LIVE.fetch_add(layout.size() as i64, std::sync::atomic::Ordering::Relaxed);
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        LIVE.fetch_sub(layout.size() as i64, std::sync::atomic::Ordering::Relaxed);
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn live() -> i64 {
    LIVE.load(std::sync::atomic::Ordering::Relaxed)
}

fn rss_kb() -> i64 {
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find(|l| l.starts_with("VmRSS:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

#[test]
#[ignore]
fn memory_over_a_million_checks() {
    run(0);
}

/// The same with every result coming back 6 s after its check started,
/// longer than the 5 s interval: checks come due while still running.
#[test]
#[ignore]
fn memory_with_late_results() {
    run(6000);
}

fn run(late: i64) {
    let held_before = live();
    let dir = std::env::temp_dir().join(format!("checkd-memory-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut objects = String::from(
        "define command {\n command_name ok\n command_line /bin/true\n}\n\
         define timeperiod {\n timeperiod_name always\n alias always\n sunday 00:00-24:00\n monday 00:00-24:00\n tuesday 00:00-24:00\n \
         wednesday 00:00-24:00\n thursday 00:00-24:00\n friday 00:00-24:00\n saturday 00:00-24:00\n}\n",
    );
    for h in 0..1000 {
        objects.push_str(&format!("define host {{\n host_name h{h}\n address 192.0.2.1\n check_command ok\n max_check_attempts 1\n check_period always\n}}\n"));
        for s in 0..10 {
            objects.push_str(&format!(
                "define service {{\n host_name h{h}\n service_description s{s}\n check_command ok\n max_check_attempts 1\n check_interval 5\n retry_interval 5\n check_period always\n}}\n"
            ));
        }
    }
    std::fs::write(dir.join("objects.cfg"), objects).unwrap();
    std::fs::write(
        dir.join("naemon.cfg"),
        "cfg_file=objects.cfg\ninterval_length=1\nexecute_host_checks=0\ncached_service_check_horizon=0\nlog_initial_states=0\n",
    )
    .unwrap();
    let loaded = checkd_config::load(Path::new(&dir.join("naemon.cfg")), &Default::default());
    let (main, objects) = (loaded.main.unwrap(), loaded.objects.unwrap());
    let mut now = 1_800_000_000_000;
    let (mut engine, _) = Engine::new(objects, &main, Box::new(checkd_core::timeperiod::Fixed(0)), now);
    let mut effects = Vec::new();
    engine.start(&mut effects);
    let mut checks = 0u64;
    let mut waiting: std::collections::VecDeque<(i64, u64)> = std::collections::VecDeque::new();
    let mut marks = Vec::new();
    while checks < 1_000_000 {
        now += 100;
        engine.advance(now, &mut effects);
        waiting.extend(effects.drain(..).filter_map(|e| match e {
            Effect::Run(job) => Some((now + late, job.id)),
            _ => None,
        }));
        let ready = waiting.iter().take_while(|(at, _)| *at <= now).count();
        let jobs: Vec<u64> = waiting.drain(..ready).map(|(_, id)| id).collect();
        for id in jobs {
            checks += 1;
            let result = JobResult { outcome: Outcome::Exited(0), stdout: "OK fine|time=0.1s".into(), stderr: String::new() };
            engine.job_done(now, id, result, &mut effects);
            effects.clear();
        }
        if checks / 200_000 > marks.len() as u64 {
            marks.push((checks, rss_kb() / 1024));
        }
        if now > 1_800_000_000_000 + 3_600_000 {
            break;
        }
    }
    println!("checks, MB: {marks:?}");
    println!("held after {checks} checks: {} bytes a service", (live() - held_before) / 10_000);
    let grown = marks.last().unwrap().1 - marks.first().unwrap().1;
    assert!(grown < 20, "the engine grew by {grown} MB over {checks} checks");
}

/// Where the memory of a configuration goes: after loading, and after the
/// engine is made from it.
#[test]
#[ignore]
fn memory_per_service() {
    let dir = std::env::temp_dir().join(format!("checkd-memory-size-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut objects = String::from(
        "define command {\n command_name ok\n command_line /bin/true\n}\n\
         define timeperiod {\n timeperiod_name always\n alias always\n monday 00:00-24:00\n}\n",
    );
    for h in 0..4000 {
        objects.push_str(&format!("define host {{\n host_name h{h}\n address 192.0.2.1\n check_command ok\n max_check_attempts 1\n check_period always\n}}\n"));
        for s in 0..10 {
            objects.push_str(&format!(
                "define service {{\n host_name h{h}\n service_description s{s}\n check_command ok\n max_check_attempts 1\n check_interval 5\n check_period always\n}}\n"
            ));
        }
    }
    std::fs::write(dir.join("objects.cfg"), objects).unwrap();
    std::fs::write(dir.join("naemon.cfg"), "cfg_file=objects.cfg\ninterval_length=1\n").unwrap();
    let before = rss_kb();
    let held_before = live();
    let loaded = checkd_config::load(Path::new(&dir.join("naemon.cfg")), &Default::default());
    let after_load = rss_kb();
    let (main, objects) = (loaded.main.unwrap(), loaded.objects.unwrap());
    println!("held after loading: {} bytes a service", (live() - held_before) / 40_000);
    let (engine, _) = Engine::new(objects, &main, Box::new(checkd_core::timeperiod::Fixed(0)), 1_800_000_000_000);
    let after_engine = rss_kb();
    println!("held by the program: {} bytes a service", (live() - held_before) / 40_000);
    // What the C allocator keeps of what loading freed.
    unsafe extern "C" {
        fn malloc_trim(pad: usize) -> i32;
    }
    unsafe {
        malloc_trim(0);
    }
    let trimmed = rss_kb();
    println!("after giving freed memory back: {} bytes a service", (trimmed - before) * 1024 / 40_000);
    println!(
        "40,000 services: loading {} MB, engine {} MB more; {} bytes a service in all",
        (after_load - before) / 1024,
        (after_engine - after_load) / 1024,
        (after_engine - before) * 1024 / 40_000
    );
    drop(engine);
}
