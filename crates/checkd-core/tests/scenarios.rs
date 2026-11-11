//! Plays each scenario under `runs/scenarios` through the engine on a
//! simulated clock and compares what it logs, and when it calls which
//! plugin, with what the reference did on the same config.
//!
//! The scripted plugins of `runs/lib/plugins` are not run: this driver
//! knows what each of them does. See `runs/README.md` for what is
//! compared and how exactly.
//!
//! `SCENARIO=service-soft-hard cargo test -p checkd-core --test scenarios -- --nocapture`
//! plays one scenario and prints both sides.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque};
use std::path::{Path, PathBuf};

use checkd_core::cmdline::{self, Start};
use checkd_core::engine::{Effect, Engine, Job, JobResult, Ms, Obj, Outcome};
use checkd_core::timeperiod::Fixed;

/// How far apart the two sides may be in time.
const SLACK: Ms = 1100;
/// What happens this close to the end of a run may be on one side only.
const END: Ms = 2200;
/// Calls of one plugin closer together than this count as one.
const TWICE: Ms = 500;
/// How long it is from the start of a scripted plugin until its result
/// is read.
const PLUGIN: Ms = 2;
/// How long it is from the start of a scripted plugin until it writes its
/// line in the trace. The line is written as the plugin ends, and the
/// result is read a moment after that. The reference's trace shows a check
/// that was started just before another's result was read: it still saw
/// the old state.
const TRACE: Ms = 1;
/// A check is held back to the reference's time when that is no more than
/// this much later,
const LATE: Ms = 40;
/// or this much earlier.
const EARLY: Ms = 3;

#[derive(Default)]
struct Compare {
    /// Whether this is read for checkd's real run.
    real_run: bool,
    /// Why the scenario is not played here on the simulated clock,
    skip: Option<String>,
    /// or why checkd's real run of it is not compared.
    skip_real: Option<String>,
    /// Plugin keys whose calls are not compared.
    ignore: BTreeSet<String>,
    /// Plugin keys compared by time only, not by what they were called with.
    loose: BTreeSet<String>,
    /// Objects (`a` or `a;s`) whose log lines are not compared.
    ignore_log: BTreeSet<String>,
    /// Plugin keys that are called to look at another object. Such a call
    /// may show what checkd's call before or after it shows.
    watch: BTreeSet<String>,
    /// Calls the reference made and checkd is not to make: the key and
    /// the millisecond in the reference's trace.
    drop: BTreeSet<(String, Ms)>,
    /// First checks the reference had planned for after the end of its
    /// run: the key and the millisecond, as its saved state has it.
    first: BTreeMap<String, Ms>,
    /// The same by object, for checks that are not a scripted plugin and
    /// so leave nothing in the trace.
    first_of: BTreeMap<String, Ms>,
    /// Objects whose check is a real program or needs a real shell. The
    /// play here cannot run those: the run with real programs covers them.
    real: BTreeSet<String>,
    /// Fields (`name=value`, with `|` between them) of what a plugin key
    /// was called with that are not compared: in every call, or in the
    /// one call the reference made at the millisecond given.
    mask: BTreeMap<String, Vec<(Option<Ms>, BTreeSet<String>)>>,
}

/// Reads a scenario's `compare` file. `real` says that it is read for
/// checkd's real run and not for the play on the simulated clock.
fn read_compare(dir: &Path, real: bool) -> Compare {
    let mut compare = Compare {
        real_run: real,
        ..Compare::default()
    };
    let Ok(text) = std::fs::read_to_string(dir.join("compare")) else {
        return compare;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.splitn(3, ' ');
        let (what, first, rest) = (
            words.next().unwrap_or(""),
            words.next().unwrap_or(""),
            words.next().unwrap_or(""),
        );
        match what {
            "skip" => compare.skip = Some(format!("{first} {rest}")),
            "skip-real" => compare.skip_real = Some(format!("{first} {rest}")),
            "ignore-real" | "ignore-log-real" if !real => {}
            "ignore" | "ignore-real" => {
                compare.ignore.insert(first.to_string());
            }
            "loose" => {
                compare.loose.insert(first.to_string());
            }
            "watch" => {
                compare.watch.insert(first.to_string());
            }
            "ignore-log" | "ignore-log-real" => {
                compare.ignore_log.insert(first.to_string());
            }
            "drop" => {
                let (key, at) = first
                    .split_once('@')
                    .unwrap_or_else(|| panic!("{}: drop wants key@millisecond", dir.display()));
                let at = at
                    .parse()
                    .unwrap_or_else(|_| panic!("{}: drop wants key@millisecond", dir.display()));
                compare.drop.insert((key.to_string(), at));
            }
            "real" => {
                compare.real.insert(first.to_string());
                if !real {
                    compare.ignore_log.insert(first.to_string());
                }
            }
            "mask-real" if !real => {}
            "mask" | "mask-real" => {
                let (fields, why) = rest.split_once(' ').unwrap_or((rest, ""));
                if fields.is_empty() || why.trim().is_empty() {
                    panic!("{}: mask wants a key, fields and a reason", dir.display());
                }
                let (key, at) = match first.split_once('@') {
                    Some((key, at)) => {
                        let at = at.parse().unwrap_or_else(|_| {
                            panic!("{}: mask wants key or key@millisecond", dir.display())
                        });
                        (key, Some(at))
                    }
                    None => (first, None),
                };
                compare
                    .mask
                    .entry(key.to_string())
                    .or_default()
                    .push((at, fields.split(',').map(str::to_string).collect()));
            }
            "first" => {
                let (key, at) = first
                    .split_once('@')
                    .unwrap_or_else(|| panic!("{}: first wants key@millisecond", dir.display()));
                let at = at
                    .parse()
                    .unwrap_or_else(|_| panic!("{}: first wants key@millisecond", dir.display()));
                compare.first.insert(key.to_string(), at);
            }
            "first-of" => {
                let (object, at) = first.split_once('@').unwrap_or_else(|| {
                    panic!("{}: first-of wants object@millisecond", dir.display())
                });
                let at = at.parse().unwrap_or_else(|_| {
                    panic!("{}: first-of wants object@millisecond", dir.display())
                });
                compare.first_of.insert(object.to_string(), at);
            }
            other => panic!("{}: unknown line '{other}' in compare", dir.display()),
        }
    }
    compare
}

#[derive(Clone, Debug)]
struct Call {
    at: Ms,
    key: String,
    what: String,
}

#[derive(Clone, Debug)]
struct Line {
    second: i64,
    text: String,
}

/// The scripted plugins, without the scripts.
struct Plugins {
    start: Ms,
    fixtures: PathBuf,
    counts: BTreeMap<String, u32>,
    calls: Vec<Call>,
    problems: Vec<String>,
}

impl Plugins {
    /// What the command does: how long it takes and how it ends.
    fn play(&mut self, job: &Job, now: Ms) -> (Ms, JobResult) {
        let done = |code: i32, stdout: String| {
            (
                PLUGIN,
                JobResult {
                    outcome: Outcome::Exited(code),
                    stdout,
                    stderr: String::new(),
                },
            )
        };
        let (argv, direct) = match cmdline::plan(&job.line) {
            Start::Direct { argv, .. } => (argv, true),
            Start::Shell => match shell_words(&job.line) {
                Some(argv) if !argv.is_empty() => (argv, false),
                _ => {
                    self.problems
                        .push(format!("needs a shell: {}", job.line));
                    return done(3, String::new());
                }
            },
        };
        let Some(plugin) = argv[0].strip_prefix("/case/plugins/") else {
            self.problems
                .push(format!("not a scripted plugin: {}", job.line));
            return done(3, String::new());
        };
        if !matches!(plugin, "seq" | "at" | "args" | "raw") || argv.len() < 2 {
            // A program that is not there.
            if direct {
                let why = format!(
                    "execvp({}, ...) failed. errno is 2: No such file or directory",
                    argv[0]
                );
                return (
                    PLUGIN,
                    JobResult {
                        outcome: Outcome::Failed(why),
                        stdout: String::new(),
                        stderr: String::new(),
                    },
                );
            }
            return (
                PLUGIN,
                JobResult {
                    outcome: Outcome::Exited(127),
                    stdout: String::new(),
                    stderr: format!("/bin/sh: 1: {}: not found", argv[0]),
                },
            );
        }
        let key = argv[1].clone();
        let rest = &argv[2..];
        let elapsed = now - self.start;
        let mut call = |what: String| {
            self.calls.push(Call {
                at: elapsed + TRACE,
                key: key.clone(),
                what,
            })
        };
        match plugin {
            "seq" => {
                let n = self.counts.entry(key.clone()).or_insert(0);
                *n += 1;
                let n = *n;
                let mut left = n as i64;
                let mut code = "3".to_string();
                let mut text = String::new();
                // The script gets its steps as words.
                for step in rest.iter().flat_map(|arg| arg.split(' ')).filter(|s| !s.is_empty()) {
                    let (step, words) = match step.split_once(':') {
                        Some((step, words)) => (step, words.to_string()),
                        None => (step, String::new()),
                    };
                    text = words;
                    let (this, count) = match step.split_once('x') {
                        Some((this, count)) => (this, count.parse().unwrap_or(1)),
                        None => (step, 1),
                    };
                    code = this.to_string();
                    left -= count;
                    if left <= 0 {
                        break;
                    }
                }
                call(code.clone());
                if text.is_empty() {
                    text = format!("{key} call {n} gives {code}");
                }
                done(code.parse().unwrap_or(3), format!("{text}\n"))
            }
            "at" => {
                let age = elapsed / 1000;
                let mut steps = rest.iter().flat_map(|arg| arg.split(' ')).filter(|s| !s.is_empty());
                let mut code = steps.next().unwrap_or("3").to_string();
                for step in steps {
                    if let Some((second, then)) = step.split_once(':') {
                        if age >= second.parse().unwrap_or(i64::MAX) {
                            code = then.to_string();
                        }
                    }
                }
                call(code.clone());
                done(
                    code.parse().unwrap_or(3),
                    format!("{key} is {code} at second {age}\n"),
                )
            }
            "args" => {
                let (code, rest) = match rest {
                    [flag, code, rest @ ..] if flag == "-c" => (code.parse().unwrap_or(0), rest),
                    rest => (0, rest),
                };
                call(rest.join("|"));
                done(code, format!("{key} called\n"))
            }
            _ => {
                let code = rest.first().map(String::as_str).unwrap_or("3");
                let file = rest.get(1).map(String::as_str).unwrap_or("-");
                let wait: f64 = rest.get(2).and_then(|w| w.parse().ok()).unwrap_or(0.0);
                call(code.to_string());
                let wait = (wait * 1000.0) as Ms;
                if wait >= job.timeout {
                    return (
                        job.timeout,
                        JobResult {
                            outcome: Outcome::TimedOut,
                            stdout: String::new(),
                            stderr: String::new(),
                        },
                    );
                }
                let stdout = if file == "-" {
                    String::new()
                } else {
                    let bytes = std::fs::read(self.fixtures.join(file)).unwrap_or_default();
                    String::from_utf8_lossy(&bytes).into_owned()
                };
                let outcome = if code == "kill" {
                    Outcome::Signaled(9)
                } else {
                    Outcome::Exited(code.parse().unwrap_or(3))
                };
                (
                    wait + PLUGIN,
                    JobResult {
                        outcome,
                        stdout,
                        stderr: String::new(),
                    },
                )
            }
        }
    }
}

/// Splits a line the way a shell would, for lines that are plain words
/// and quotes. `None` for anything a shell would do more with.
fn shell_words(line: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => words.extend(word.take()),
            '\'' => {
                let text = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => text.push(c),
                    }
                }
            }
            '"' => {
                let text = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => text.push(chars.next()?),
                        '$' | '`' => return None,
                        c => text.push(c),
                    }
                }
            }
            '\\' => word.get_or_insert_with(String::new).push(chars.next()?),
            '&' | '|' | ';' | '<' | '>' | '(' | ')' | '$' | '`' | '*' | '?' | '~' | '#' => {
                return None;
            }
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    words.extend(word);
    Some(words)
}

struct Expected {
    start_ns: i64,
    lines: Vec<Line>,
    calls: Vec<Call>,
    /// The second in which the reference was told to stop.
    stopped: i64,
}

fn read_expected(dir: &Path) -> Expected {
    read_run(&dir.join("expected")).expect("expected/start")
}

/// What a run left behind: its log, its trace of plugin calls and when
/// it began.
fn read_run(dir: &Path) -> Option<Expected> {
    let read = |name: &str| {
        String::from_utf8_lossy(&std::fs::read(dir.join(name)).unwrap_or_default()).into_owned()
    };
    let start_ns: i64 = read("start").trim().parse().ok()?;
    let mut lines = Vec::new();
    let mut stopped = 0;
    for line in read("naemon.log").lines() {
        let Some((second, text)) = line.strip_prefix('[').and_then(|l| l.split_once("] ")) else {
            continue;
        };
        let second: i64 = second.parse().unwrap_or(0);
        if text.starts_with("Caught ") {
            stopped = second;
        }
        lines.push(Line {
            second,
            text: text.to_string(),
        });
    }
    let mut calls = Vec::new();
    for line in read("trace").lines() {
        let mut parts = line.splitn(3, ' ');
        let (Some(at), Some(key)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Ok(at) = at.parse() else { continue };
        calls.push(Call {
            at,
            key: key.to_string(),
            what: parts.next().unwrap_or("").to_string(),
        });
    }
    Some(Expected {
        start_ns,
        lines,
        calls,
        stopped,
    })
}

/// The object a log line is about, for the lines that are compared.
fn about(text: &str) -> Option<String> {
    let (label, rest) = text.split_once(": ")?;
    let fields = |n: usize| rest.split(';').take(n).collect::<Vec<_>>().join(";");
    match label {
        "INITIAL HOST STATE" | "HOST ALERT" | "HOST FLAPPING ALERT" | "HOST EVENT HANDLER"
        | "GLOBAL HOST EVENT HANDLER" | "HOST INFO" => Some(fields(1)),
        "INITIAL SERVICE STATE"
        | "SERVICE ALERT"
        | "SERVICE FLAPPING ALERT"
        | "SERVICE EVENT HANDLER"
        | "GLOBAL SERVICE EVENT HANDLER"
        | "SERVICE INFO" => Some(fields(2)),
        "Warning" if rest.starts_with("Return code of ") => {
            let quoted: Vec<&str> = rest.split('\'').collect();
            if rest.contains("for check of service '") {
                Some(format!("{};{}", quoted.get(3)?, quoted.get(1)?))
            } else {
                Some(quoted.get(1)?.to_string())
            }
        }
        _ => None,
    }
}

fn by_object(lines: &[Line], ignore: &BTreeSet<String>) -> BTreeMap<String, Vec<Line>> {
    let mut map: BTreeMap<String, Vec<Line>> = BTreeMap::new();
    for line in lines {
        if let Some(object) = about(&line.text) {
            if !ignore.contains(&object) {
                map.entry(object).or_default().push(line.clone());
            }
        }
    }
    map
}

fn by_key(calls: &[Call], ignore: &BTreeSet<String>) -> BTreeMap<String, Vec<Call>> {
    let mut map: BTreeMap<String, Vec<Call>> = BTreeMap::new();
    for call in calls {
        if ignore.contains(&call.key) {
            continue;
        }
        let list = map.entry(call.key.clone()).or_default();
        // Two checks of a host within a moment of each other count as
        // one. Whether a check on demand comes just before the planned
        // one has finished or just after is a matter of milliseconds.
        if list
            .last()
            .is_some_and(|last| call.at - last.at < TWICE && last.what == call.what)
        {
            continue;
        }
        list.push(call.clone());
    }
    map
}

struct Played {
    lines: Vec<Line>,
    calls: Vec<Call>,
    problems: Vec<String>,
    end: Ms,
    start: Ms,
}

/// The engine for a scenario, each object's plugin key, and when the
/// reference first checked each object, in the order it did.
struct Setup {
    engine: Engine,
    start: Ms,
    end: Ms,
    /// How many seconds the reference ran.
    ran: i64,
    keys: BTreeMap<Obj, String>,
    /// Milliseconds after the start.
    firsts: Vec<(Ms, Obj)>,
    effects: Vec<Effect>,
}

fn setup(dir: &Path, expected: &Expected) -> Result<Setup, String> {
    let loaded = checkd_config::load(&dir.join("naemon.cfg"), &Default::default());
    let (Some(mut main), Some(objects)) = (loaded.main, loaded.objects) else {
        return Err(format!("config does not load: {:?}", loaded.diags));
    };
    // What the oracle's runner sets for every case.
    main.path = PathBuf::from("/case/naemon.cfg");
    for (name, value) in [
        ("log_file", "/tmp/oracle/naemon.log"),
        ("temp_path", "/tmp/oracle"),
        ("status_file", "/tmp/oracle/status.dat"),
        ("state_retention_file", "/tmp/oracle/retention.dat"),
        ("object_cache_file", "/tmp/oracle/objects.cache"),
        ("command_file", "/tmp/oracle/naemon.cmd"),
    ] {
        main.settings.push((name.to_string(), value.to_string()));
    }

    let start = expected.start_ns / 1_000_000;
    // The reference is stopped a whole number of seconds after the start.
    let ran = ((expected.stopped + 1) * 1000 - start) / 1000;
    let end = start + ran * 1000 + 30;

    let (mut engine, _warnings) = Engine::new(objects, &main, Box::new(Fixed(0)), start);
    let mut effects = Vec::new();
    engine.start(&mut effects);

    // Each object's first check is when the reference first checked it,
    // and where two are due at once they go in the reference's order.
    let mut first: BTreeMap<&str, (Ms, usize)> = BTreeMap::new();
    for (place, call) in expected.calls.iter().enumerate() {
        first.entry(&call.key).or_insert((call.at, place));
    }
    let compare = read_compare(dir, false);
    for (key, at) in &compare.first {
        first.insert(key, (*at, usize::MAX));
    }
    let objects: Vec<Obj> = (0..engine.hosts().len())
        .map(Obj::Host)
        .chain((0..engine.services().len()).map(Obj::Service))
        .collect();
    let mut firsts: Vec<((Ms, usize), Obj)> = Vec::new();
    let mut keys: BTreeMap<Obj, String> = BTreeMap::new();
    for obj in objects {
        let Some(line) = engine.check_line(obj, start) else {
            continue;
        };
        let argv = match cmdline::plan(&line) {
            Start::Direct { argv, .. } => argv,
            Start::Shell => shell_words(&line).unwrap_or_default(),
        };
        let scripted = argv.first().is_some_and(|program| {
            ["seq", "at", "args", "raw"]
                .iter()
                .any(|name| program.strip_prefix("/case/plugins/") == Some(name))
        });
        let Some(key) = argv.get(1).filter(|_| scripted) else {
            continue;
        };
        if let Some(at) = first.get(key.as_str()) {
            firsts.push((*at, obj));
        }
        keys.insert(obj, key.clone());
    }
    firsts.sort();
    Ok(Setup {
        engine,
        start,
        end,
        ran,
        keys,
        firsts: firsts.into_iter().map(|((at, _), obj)| (at, obj)).collect(),
        effects,
    })
}

fn play(dir: &Path, expected: &Expected) -> Result<Played, String> {
    let Setup {
        mut engine,
        start,
        end,
        keys,
        firsts,
        mut effects,
        ..
    } = setup(dir, expected)?;
    let compare = read_compare(dir, false);
    let mut plugins = Plugins {
        start,
        fixtures: dir.join("fixtures"),
        counts: BTreeMap::new(),
        calls: Vec::new(),
        problems: Vec::new(),
    };
    for (at, obj) in firsts {
        engine.set_first_check(obj, start + (at - TRACE).max(0));
    }

    // The reference is a few milliseconds late with every check, and its
    // next check of the object counts from when it really started. So
    // that the two do not drift apart, a check here is held back to the
    // moment the reference started its own, where that is close by.
    let mut theirs: BTreeMap<&str, VecDeque<Ms>> = BTreeMap::new();
    for call in &expected.calls {
        theirs.entry(&call.key).or_default().push_back(start + call.at - TRACE);
    }
    let mut held: BTreeSet<(Obj, Ms)> = BTreeSet::new();
    // Two results that come in at one moment are read in the order the
    // reference's trace has the two calls in.
    let mut places: BTreeMap<(&str, usize), usize> = BTreeMap::new();
    let mut nth: BTreeMap<&str, usize> = BTreeMap::new();
    for (place, call) in expected.calls.iter().enumerate() {
        let n = nth.entry(&call.key).or_default();
        places.insert((&call.key, *n), place);
        *n += 1;
    }
    let mut made: BTreeMap<String, usize> = BTreeMap::new();

    let mut lines = Vec::new();
    let mut waiting: BinaryHeap<Reverse<(Ms, usize, u64)>> = BinaryHeap::new();
    let mut results: BTreeMap<u64, JobResult> = BTreeMap::new();
    let mut now = start;
    loop {
        for effect in effects.drain(..) {
            match effect {
                Effect::Log(text) => lines.push(Line {
                    second: now.div_euclid(1000),
                    text,
                }),
                Effect::Run(job) => {
                    let before = plugins.calls.len();
                    let problems = plugins.problems.len();
                    let (takes, result) = plugins.play(&job, now);
                    if compare.real.contains(&engine.log_name(job.obj)) {
                        plugins.problems.truncate(problems);
                    }
                    let mut place = usize::MAX;
                    if let Some(call) = plugins.calls.get(before) {
                        let n = made.entry(call.key.clone()).or_default();
                        place = places.get(&(call.key.as_str(), *n)).copied().unwrap_or(place);
                        *n += 1;
                    }
                    waiting.push(Reverse((now + takes, place, job.id)));
                    results.insert(job.id, result);
                }
            }
        }
        let wake = loop {
            let Some((at, obj)) = engine.next_due() else {
                break None;
            };
            if held.contains(&(obj, at)) {
                break Some(at);
            }
            let Some(times) = keys.get(&obj).and_then(|key| theirs.get_mut(key.as_str())) else {
                break Some(at);
            };
            while times.front().is_some_and(|&time| time < at - EARLY) {
                times.pop_front();
            }
            match times.front() {
                Some(&time) if time <= at + LATE => {
                    times.pop_front();
                    let time = time.max(now);
                    engine.set_first_check(obj, time);
                    held.insert((obj, time));
                }
                _ => break Some(at),
            }
        };
        let done = waiting.peek().map(|Reverse((at, _, _))| *at);
        let next = match (wake, done) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => break,
        };
        if next > end {
            break;
        }
        now = next.max(now);
        if done == Some(next) {
            let Reverse((_, _, id)) = waiting.pop().unwrap();
            let result = results.remove(&id).unwrap();
            engine.job_done(now, id, result, &mut effects);
        } else {
            engine.advance(now, &mut effects);
            held.retain(|&(_, at)| at > now);
        }
    }
    Ok(Played {
        lines,
        calls: plugins.calls,
        problems: plugins.problems,
        end,
        start,
    })
}

/// Compares two lists that should hold the same things at about the same
/// times. Things near the end of the run may be missing on one side.
fn compare_lists<T>(
    what: &str,
    expected: &[T],
    actual: &[T],
    time_e: impl Fn(&T) -> Ms,
    time_a: impl Fn(&T) -> Ms,
    text: impl Fn(&T) -> String,
    end: Ms,
    out: &mut Vec<String>,
) {
    let both = expected.len().min(actual.len());
    for at in 0..both {
        let (e, a) = (&expected[at], &actual[at]);
        if text(e) != text(a) {
            out.push(format!(
                "{what}: entry {} differs\n      reference {:>6} {}\n      checkd    {:>6} {}",
                at + 1,
                time_e(e),
                text(e),
                time_a(a),
                text(a)
            ));
            return;
        }
        if (time_e(e) - time_a(a)).abs() > SLACK {
            out.push(format!(
                "{what}: entry {} at another time\n      reference {:>6} {}\n      checkd    {:>6} {}",
                at + 1,
                time_e(e),
                text(e),
                time_a(a),
                text(a)
            ));
            return;
        }
    }
    let (side, extra, time): (_, _, &dyn Fn(&T) -> Ms) = if expected.len() > both {
        ("reference", &expected[both..], &time_e)
    } else {
        ("checkd", &actual[both..], &time_a)
    };
    if let Some(early) = extra.iter().find(|entry| time(entry) < end - END) {
        out.push(format!(
            "{what}: only {side} has {:>6} {} ({} more in all)",
            time(early),
            text(early),
            extra.len()
        ));
    }
}

fn check(dir: &Path, verbose: bool) -> Result<Vec<String>, String> {
    let compare = read_compare(dir, false);
    if let Some(why) = compare.skip {
        return Err(why);
    }
    let expected = read_expected(dir);
    let played = play(dir, &expected).map_err(|why| format!("FAILED {why}"))?;
    Ok(differences(&compare, &expected, &played, verbose))
}

/// Whether two lists of what a watching check saw can come from one
/// course of events.
///
/// A check that looks at another object is started within a millisecond
/// or two of that object's result being read. Which comes first is not
/// the same every time, on either side: call k sees the watched object as
/// it was before its k-th result or after it. The two lists are alike
/// when one series of states exists that both can have seen in that way.
///
/// This lets through any single call that differs between two calls that
/// agree. What is still held to: the calls are as many and at the same
/// times, and two differences in a row must fit one series.
fn watched_alike(want: &[Call], have: &[Call]) -> bool {
    let both = want.len().min(have.len());
    let seen = |k: usize| [&want[k].what, &have[k].what];
    if both == 0 {
        return true;
    }
    // The states the object can have been in before the first call.
    let mut reach: Vec<&String> = seen(0).to_vec();
    for k in 0..both {
        let [r, c] = seen(k);
        let mut candidates = seen(k).to_vec();
        if k + 1 < both {
            candidates.extend(seen(k + 1));
        }
        candidates.dedup();
        let next: Vec<&String> = candidates
            .into_iter()
            .filter(|&after| {
                reach
                    .iter()
                    .any(|&before| (r == before || r == after) && (c == before || c == after))
            })
            .collect();
        if next.is_empty() {
            return false;
        }
        reach = next;
    }
    true
}

/// What the two sides do not agree on.
fn differences(compare: &Compare, expected: &Expected, played: &Played, verbose: bool) -> Vec<String> {
    let mut out = Vec::new();
    out.extend(played.problems.iter().cloned());

    // A line's time is known to the second only.
    let rel_to = |start: Ms| move |second: i64| second * 1000 - start + 500;
    let rel_e = rel_to(expected.start_ns / 1_000_000);
    let rel_a = rel_to(played.start);
    let end = played.end - played.start;

    let want = by_object(&expected.lines, &compare.ignore_log);
    let have = by_object(&played.lines, &compare.ignore_log);
    let objects: BTreeSet<&String> = want.keys().chain(have.keys()).collect();
    let none = Vec::new();
    for object in objects {
        compare_lists(
            &format!("log of {object}"),
            want.get(object).unwrap_or(&none),
            have.get(object).unwrap_or(&none),
            |line| rel_e(line.second),
            |line| rel_a(line.second),
            |line| line.text.clone(),
            end,
            &mut out,
        );
    }

    let kept: Vec<Call> = expected
        .calls
        .iter()
        .filter(|call| !compare.drop.contains(&(call.key.clone(), call.at)))
        .cloned()
        .collect();
    let want = by_key(&kept, &compare.ignore);
    let have = by_key(&played.calls, &compare.ignore);
    let keys: BTreeSet<&String> = want.keys().chain(have.keys()).collect();
    let none = Vec::new();
    for key in keys {
        let loose = compare.loose.contains(key);
        let mask = compare.mask.get(key.as_str()).map(Vec::as_slice).unwrap_or(&[]);
        let reference = want.get(key).unwrap_or(&none);
        let want = &tidy(reference, mask, reference);
        let mut have = tidy(have.get(key).unwrap_or(&none), mask, reference);
        if compare.watch.contains(key) && compare.real_run {
            // In real time both sides start a watching check a
            // millisecond or two from the watched object's result.
            if watched_alike(want, &have) {
                for (call, wanted) in have.iter_mut().zip(want) {
                    call.what = wanted.what.clone();
                }
            }
        } else if compare.watch.contains(key) {
            // On the simulated clock only the reference does: a call of
            // checkd's may show what the reference's call before or after
            // it shows, or the reference's call what checkd's neighbours
            // show.
            let seen: Vec<String> = have.iter().map(|call| call.what.clone()).collect();
            let near = |list: &dyn Fn(usize) -> Option<String>, at: usize, what: &str| {
                [at.wrapping_sub(1), at + 1]
                    .iter()
                    .any(|&n| list(n).as_deref() == Some(what))
            };
            for (at, call) in have.iter_mut().enumerate() {
                let Some(wanted) = want.get(at) else { break };
                if call.what == wanted.what {
                    continue;
                }
                let ours = |n: usize| seen.get(n).cloned();
                let reference = |n: usize| want.get(n).map(|call| call.what.clone());
                if near(&ours, at, &wanted.what) || near(&reference, at, &call.what) {
                    call.what = wanted.what.clone();
                }
            }
        }
        compare_lists(
            &format!("calls of {key}"),
            want,
            &have,
            |call| call.at,
            |call| call.at,
            |call| if loose { String::new() } else { call.what.clone() },
            end,
            &mut out,
        );
    }

    if verbose {
        println!("--- reference log");
        for line in &expected.lines {
            if about(&line.text).is_some() {
                println!("{:>7} {}", rel_e(line.second), line.text);
            }
        }
        println!("--- checkd log");
        for line in &played.lines {
            println!("{:>7} {}", rel_a(line.second), line.text);
        }
        let short = |what: &str| what.chars().take(60).collect::<String>();
        println!("--- reference calls");
        for call in &expected.calls {
            println!("{:>7} {} {}", call.at, call.key, short(&call.what));
        }
        println!("--- checkd calls");
        for call in &played.calls {
            println!("{:>7} {} {}", call.at, call.key, short(&call.what));
        }
    }
    out
}

/// The calls with the fields that are not compared blanked, and with each
/// problem id, which is drawn at random on both sides, replaced by its
/// number in order of appearance.
fn tidy(calls: &[Call], mask: &[(Option<Ms>, BTreeSet<String>)], reference: &[Call]) -> Vec<Call> {
    let mut ids: Vec<String> = Vec::new();
    calls
        .iter()
        .enumerate()
        .map(|(n, call)| {
            let masked = |name: &str| {
                mask.iter().any(|(at, fields)| {
                    fields.contains(name)
                        && at.is_none_or(|at| reference.get(n).is_some_and(|call| call.at == at))
                })
            };
            let fields: Vec<String> = call
                .what
                .split('|')
                .map(|field| {
                    let Some((name, value)) = field.split_once('=') else {
                        return field.to_string();
                    };
                    if masked(name) {
                        return format!("{name}=*");
                    }
                    if is_uuid(value) {
                        let n = ids.iter().position(|id| id == value).unwrap_or_else(|| {
                            ids.push(value.to_string());
                            ids.len() - 1
                        });
                        return format!("{name}=<id {}>", n + 1);
                    }
                    field.to_string()
                })
                .collect();
            Call { what: fields.join("|"), ..call.clone() }
        })
        .collect()
}

fn is_uuid(text: &str) -> bool {
    text.len() == 36
        && text.chars().enumerate().all(|(at, c)| match at {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// The scenario directories, or those that `SCENARIO` names, with commas
/// between them. The second part of the answer says that there is one
/// only, which is then shown in full.
fn scenario_dirs() -> (Vec<(String, PathBuf)>, bool) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runs/scenarios");
    let only: Option<Vec<String>> = std::env::var("SCENARIO")
        .ok()
        .map(|names| names.split(',').map(str::to_string).collect());
    let mut dirs: Vec<(String, PathBuf)> = std::fs::read_dir(&root)
        .expect("runs/scenarios")
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.join("naemon.cfg").exists())
        .map(|path| (path.file_name().unwrap().to_string_lossy().into_owned(), path))
        .filter(|(name, _)| only.as_ref().is_none_or(|only| only.contains(name)))
        .collect();
    dirs.sort();
    (dirs, only.is_some_and(|only| only.len() == 1))
}

/// For `runs/real`: writes, for each scenario, how many seconds the
/// reference ran, when it began and when it first checked each object,
/// to the directory that `CHECKD_REAL_PLAN` names. Does nothing without
/// it.
#[test]
fn plan() {
    let Some(out) = std::env::var_os("CHECKD_REAL_PLAN").map(PathBuf::from) else {
        return;
    };
    for (name, dir) in scenario_dirs().0 {
        let expected = read_expected(&dir);
        let setup = setup(&dir, &expected).unwrap_or_else(|why| panic!("{name}: {why}"));
        let start = expected.start_ns / 1_000_000;
        let compare = read_compare(&dir, true);
        let mut firsts: Vec<(Ms, String)> = setup
            .firsts
            .iter()
            .map(|(at, obj)| (start + (at - TRACE).max(0), setup.engine.log_name(*obj)))
            .collect();
        // A check that is not a scripted plugin leaves nothing in the
        // trace. Where the reference's log has a line about the object,
        // the first one says when its first check was, to the second.
        let engine = &setup.engine;
        let objects = (0..engine.hosts().len())
            .map(Obj::Host)
            .chain((0..engine.services().len()).map(Obj::Service));
        for obj in objects {
            if setup.firsts.iter().any(|(_, known)| *known == obj) {
                continue;
            }
            let name = engine.log_name(obj);
            if let Some(at) = compare.first_of.get(&name) {
                firsts.push((start + at, name));
                continue;
            }
            let line = expected
                .lines
                .iter()
                .filter(|line| !line.text.starts_with("INITIAL "))
                .find(|line| about(&line.text).as_deref() == Some(name.as_str()));
            if let Some(line) = line {
                firsts.push(((line.second * 1000 + 500).max(start), name));
            }
        }
        firsts.sort_by_key(|(at, _)| *at);
        let firsts: String = firsts.iter().map(|(at, name)| format!("{at} {name}\n")).collect();
        let out = out.join(&name);
        std::fs::create_dir_all(&out).expect("the plan directory");
        std::fs::write(out.join("firsts"), firsts).expect("firsts");
        std::fs::write(out.join("seconds"), format!("{}\n", setup.ran)).expect("seconds");
        std::fs::write(out.join("start"), format!("{start}\n")).expect("start");
    }
}

/// For `runs/real`: compares what checkd itself did on each scenario, on
/// the real clock and with the real plugins, with the reference's
/// recording. The runs are under the directory that `CHECKD_REAL` names.
/// Does nothing without it.
#[test]
fn real() {
    let Some(runs) = std::env::var_os("CHECKD_REAL").map(PathBuf::from) else {
        return;
    };
    let (dirs, verbose) = scenario_dirs();
    let (mut compared, mut failed) = (0, 0);
    for (name, dir) in dirs {
        let compare = read_compare(&dir, true);
        if let Some(why) = &compare.skip_real {
            println!("{name}: not compared: {why}");
            continue;
        }
        let expected = read_expected(&dir);
        let Some(run) = read_run(&runs.join(&name).join("out")) else {
            failed += 1;
            println!("{name}: no run of checkd");
            continue;
        };
        // checkd was told to take its start for the reference's, so its
        // log is in the reference's time. The trace counts from the
        // start on both sides.
        let start = expected.start_ns / 1_000_000;
        let ran = ((expected.stopped + 1) * 1000 - start) / 1000;
        let played = Played {
            lines: run.lines,
            calls: run.calls,
            problems: Vec::new(),
            end: start + ran * 1000 + 30,
            start,
        };
        compared += 1;
        let differences = differences(&compare, &expected, &played, verbose);
        if differences.is_empty() {
            println!("{name}: same");
        } else {
            failed += 1;
            println!("{name}: {} differences", differences.len());
            for difference in differences.iter().take(12) {
                println!("   {difference}");
            }
        }
    }
    println!("{compared} real runs compared, {failed} differ");
    assert_eq!(failed, 0);
}

#[test]
fn scenarios() {
    let (dirs, verbose) = scenario_dirs();
    let mut failed = 0;
    let mut played = 0;
    for (name, dir) in dirs {
        match check(&dir, verbose) {
            Err(why) if why.starts_with("FAILED") => {
                failed += 1;
                println!("{name}: {why}");
            }
            Err(why) => println!("{name}: not played here: {why}"),
            Ok(differences) => {
                played += 1;
                if differences.is_empty() {
                    println!("{name}: same");
                } else {
                    failed += 1;
                    println!("{name}: {} differences", differences.len());
                    for difference in differences.iter().take(12) {
                        println!("   {difference}");
                    }
                }
            }
        }
    }
    println!("{played} scenarios played, {failed} differ");
    assert_eq!(failed, 0);
}

#[test]
fn watching_checks() {
    let calls = |what: &[&str]| -> Vec<Call> {
        what.iter()
            .enumerate()
            .map(|(n, what)| Call {
                at: n as Ms * 1000,
                key: "spy".to_string(),
                what: what.to_string(),
            })
            .collect()
    };
    let alike = |a: &[&str], b: &[&str]| watched_alike(&calls(a), &calls(b));
    // One side saw each result a call later than the other.
    assert!(alike(&["1", "2", "3", "4"], &["0", "1", "2", "3"]));
    // Each side was late once, at different calls.
    assert!(alike(&["1", "1", "3", "4"], &["1", "2", "2", "4"]));
    assert!(alike(&["6.12", "5.99", "5.86", "5.59"], &["6.12", "5.99", "5.72", "5.59"]));
    // Two differences in a row that no one series of states explains.
    assert!(!alike(&["1", "2", "3", "4", "5"], &["1", "9", "8", "4", "5"]));
    assert!(!alike(&["1", "2", "3", "4"], &["4", "3", "2", "1"]));
}
