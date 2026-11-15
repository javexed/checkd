//! Where a number is going: "the disk is full in four days".
//!
//!     checkd:trend <host>/<service> <label> [--full <n>] [--warn <days>] [--crit <days>] [--over <hours>]
//!
//! The engine reads the number `<label>` from the performance data of
//! another service each time this check is due, keeps what it read for
//! `--over` hours (24), and says when the number reaches `--full` (or
//! the maximum the performance data names) if it goes on as it has. See
//! `docs/trend.md`.

use std::collections::{HashMap, VecDeque};

use checkd_core::engine::{Engine, JobResult, Ms, Outcome};

pub fn is_trend(line: &str) -> bool {
    checkd_core::cmdline::own_words(line).first().map(String::as_str) == Some("checkd:trend")
}

/// The most that is kept of one number.
const MOST: usize = 3000;
/// Before this much has been watched, nothing is said of where it goes.
const SHORTEST: Ms = 30 * 60 * 1000;

/// The longest a trend watches (`--over`), and so the oldest reading the
/// store keeps.
pub const LONGEST: Ms = 60 * 24 * 3_600_000;

/// Readings to write (line, when, value), and for each trend that dropped
/// some, the time of the oldest it keeps.
pub type Changes = (Vec<(String, Ms, f64)>, Vec<(String, Ms)>);

#[derive(Default)]
pub struct Trends {
    /// By the check's own line: when, and what the number was.
    seen: HashMap<String, VecDeque<(Ms, f64)>>,
    /// What changed since `changes` was last asked: readings added, and
    /// the trends that dropped old readings, with the oldest they keep.
    added: Vec<(String, Ms, f64)>,
    trimmed: HashMap<String, Ms>,
}

fn answer(code: i32, text: String, perfdata: &str) -> JobResult {
    let word = ["OK", "WARNING", "CRITICAL", "UNKNOWN"][code as usize];
    let bar = if perfdata.is_empty() { String::new() } else { format!("|{perfdata}") };
    JobResult { outcome: Outcome::Exited(code), stdout: format!("TREND {word} - {text}{bar}\n"), stderr: String::new() }
}

/// A number of the performance data, and the maximum it names, if any:
/// `label=value[unit];warn;crit;min;max`.
pub fn number(perfdata: &str, label: &str) -> Option<(f64, Option<f64>, String)> {
    // A label with spaces is in single quotes.
    let mut rest = perfdata.trim();
    while !rest.is_empty() {
        let (name, after) = match rest.strip_prefix('\'') {
            Some(quoted) => {
                let end = quoted.find('\'')?;
                (&quoted[..end], quoted[end + 1..].strip_prefix('=')?)
            }
            None => {
                let end = rest.find('=')?;
                (&rest[..end], &rest[end + 1..])
            }
        };
        let end = after.find(char::is_whitespace).unwrap_or(after.len());
        let (fields, next) = (&after[..end], after[end..].trim_start());
        if name == label {
            let mut parts = fields.split(';');
            let leading = |text: &str| -> Option<f64> {
                let end = text.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e' || c == 'E')).unwrap_or(text.len());
                text[..end].parse().ok()
            };
            let first = parts.next()?;
            let value = leading(first)?;
            // The unit is what follows the number: `B`, `MB`, `%`, `s`.
            let unit: String = first.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e' || c == 'E').chars().filter(|c| c.is_ascii_alphabetic() || *c == '%').collect();
            let most = parts.nth(3).and_then(leading);
            return Some((value, most, unit));
        }
        rest = next;
    }
    None
}

/// How much the number grows a day, by the line that fits what was seen
/// best. `None` with fewer than two different moments.
pub fn a_day(seen: &VecDeque<(Ms, f64)>) -> Option<f64> {
    let n = seen.len() as f64;
    let first = seen.front()?.0;
    let days = |at: Ms| (at - first) as f64 / 86_400_000.0;
    let (mean_t, mean_v) = (seen.iter().map(|(t, _)| days(*t)).sum::<f64>() / n, seen.iter().map(|(_, v)| *v).sum::<f64>() / n);
    let spread: f64 = seen.iter().map(|(t, _)| (days(*t) - mean_t).powi(2)).sum();
    if spread <= 0.0 {
        return None;
    }
    Some(seen.iter().map(|(t, v)| (days(*t) - mean_t) * (v - mean_v)).sum::<f64>() / spread)
}

/// A number as a person reads it: three figures, and from ten thousand
/// on in thousands (k), millions (M), and so on.
fn rounded(value: f64) -> String {
    let mut scaled = value;
    let mut unit = "";
    if value.abs() >= 10_000.0 {
        for next in ["k", "M", "G", "T", "P"] {
            scaled /= 1000.0;
            unit = next;
            if scaled.abs() < 1000.0 {
                break;
            }
        }
    }
    let digits = if scaled.abs() >= 100.0 { 0 } else if scaled.abs() >= 10.0 { 1 } else { 2 };
    format!("{scaled:.digits$}{unit}")
}

impl Trends {
    /// Trends as the store kept them: readings by line, oldest first.
    pub fn restored(readings: Vec<(String, Ms, f64)>) -> Trends {
        let mut trends = Trends::default();
        for (line, at, value) in readings {
            trends.seen.entry(line).or_default().push_back((at, value));
        }
        trends
    }

    /// What is to be written: the readings added, and for each trend that
    /// dropped some, the time of the oldest it keeps.
    pub fn changes(&mut self) -> Changes {
        (std::mem::take(&mut self.added), self.trimmed.drain().collect())
    }

    pub fn evaluate(&mut self, engine: &Engine, line: &str, now: Ms) -> JobResult {
        let words = checkd_core::cmdline::own_words(line);
        let (mut of, mut label) = (None, None);
        let (mut full, mut warn, mut crit, mut over) = (None, 7.0, 2.0, 24.0);
        let mut at = 1;
        while at < words.len() {
            let word = words[at].as_str();
            if let Some(option) = word.strip_prefix("--") {
                let Some(value) = words.get(at + 1).and_then(|v| v.parse::<f64>().ok()) else {
                    return answer(3, format!("--{option} needs a number"), "");
                };
                match option {
                    "full" => full = Some(value),
                    "warn" => warn = value,
                    "crit" => crit = value,
                    "over" if (1.0..=24.0 * 60.0).contains(&value) => over = value,
                    "over" => return answer(3, "--over is 1 hour to 60 days, in hours".into(), ""),
                    other => return answer(3, format!("--{other} is not an option: --full, --warn, --crit, --over"), ""),
                }
                at += 2;
                continue;
            }
            if of.is_none() {
                of = Some(word.to_string());
            } else if label.is_none() {
                label = Some(word.to_string());
            } else {
                return answer(3, format!("'{word}' is one word too many: a service, and the name of one number"), "");
            }
            at += 1;
        }
        let (Some(of), Some(label)) = (of, label) else {
            return answer(3, "a trend needs host/service and the name of a number of its performance data".into(), "");
        };
        let Some((host, service)) = of.split_once('/') else {
            return answer(3, format!("'{of}' is not host/service"), "");
        };
        let found = engine.matching(host, service);
        let [obj] = found[..] else {
            return answer(3, format!("'{of}' names {} services; a trend is of one", found.len()), "");
        };
        let item = engine.item(obj);
        if !item.checked {
            return answer(0, format!("{of} is not checked yet"), "");
        }
        let Some((value, most, unit)) = number(&item.output.perfdata, &label) else {
            return answer(3, format!("{of} has no number '{label}' in its performance data"), "");
        };
        let Some(full) = full.or(most) else {
            return answer(3, format!("'{label}' names no maximum; say what full is with --full"), "");
        };

        let key = line.trim().to_string();
        let seen = self.seen.entry(key.clone()).or_default();
        // One reading for each result of the service.
        if seen.back().is_none_or(|(at, _)| *at < item.last_update.max(1)) {
            let at = item.last_update.max(1).min(now);
            seen.push_back((at, value));
            self.added.push((key.clone(), at, value));
        }
        let oldest = now - (over * 3_600_000.0) as Ms;
        let mut dropped = false;
        while seen.len() > MOST || seen.front().is_some_and(|(at, _)| *at < oldest) {
            seen.pop_front();
            dropped = true;
        }
        if dropped {
            self.trimmed.insert(key, seen.front().map_or(now, |(at, _)| *at));
        }
        let watched = seen.back().map_or(0, |(at, _)| *at) - seen.front().map_or(0, |(at, _)| *at);
        let now_at = format!("{label} is {} of {}", rounded(value), rounded(full));
        if watched < SHORTEST {
            return answer(0, format!("{now_at}, watched for {} minutes: too short to say where it goes", watched / 60_000), "");
        }
        let Some(grows) = a_day(seen) else {
            return answer(0, format!("{now_at}, not enough is known yet"), "");
        };
        if grows <= 0.0 || value >= full {
            let text = if value >= full { format!("{now_at}: full") } else { format!("{now_at}, and not growing") };
            return answer(if value >= full { 2 } else { 0 }, text, &format!("a_day={grows:.4}{unit}"));
        }
        let days = (full - value) / grows;
        let code = if days < crit {
            2
        } else if days < warn {
            1
        } else {
            0
        };
        let when = if days >= 1000.0 {
            "more than 1000 days".to_string()
        } else if days >= 2.0 {
            format!("{days:.0} days")
        } else {
            format!("{:.0} hours", days * 24.0)
        };
        answer(
            code,
            format!("{now_at}, growing {} a day: full in {when}", rounded(grows)),
            &format!("days_left={:.2};{warn};{crit} a_day={grows:.4}{unit}", days.min(100_000.0)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_number_of_the_performance_data() {
        let perfdata = "/=4096MB;8000;9000;0;10240 'used space'=55.5%;80;90;0;100 time=0.2s";
        assert_eq!(number(perfdata, "/"), Some((4096.0, Some(10240.0), "MB".into())));
        assert_eq!(number(perfdata, "used space"), Some((55.5, Some(100.0), "%".into())));
        assert_eq!(number(perfdata, "time"), Some((0.2, None, "s".into())));
        assert_eq!(number(perfdata, "inodes"), None);
        assert_eq!(number("", "x"), None);
    }

    fn engine() -> Engine {
        let objects = "define command {\n command_name ok\n command_line /bin/true\n}\n\
             define timeperiod {\n timeperiod_name always\n alias always\n monday 00:00-24:00\n}\n\
             define host {\n host_name nas\n address 192.0.2.1\n check_command ok\n max_check_attempts 1\n check_period always\n}\n\
             define service {\n host_name nas\n service_description disk\n check_command ok\n max_check_attempts 1\n check_period always\n}\n";
        let dir = std::env::temp_dir().join(format!("checkd-trend-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("objects.cfg"), objects).unwrap();
        std::fs::write(dir.join("naemon.cfg"), "cfg_file=objects.cfg\n").unwrap();
        let loaded = checkd_config::load(&dir.join("naemon.cfg"), &Default::default());
        let (Some(main), Some(objects)) = (loaded.main, loaded.objects) else { panic!("{:?}", loaded.diags.errors) };
        Engine::new(objects, &main, Box::new(checkd_core::timeperiod::Fixed(0)), 1_800_000_000_000).0
    }

    #[test]
    fn when_a_disk_is_full() {
        let mut engine = engine();
        let mut trends = Trends::default();
        let start: Ms = 1_800_000_000_000;
        let line = "checkd:trend nas/disk / --warn 7 --crit 2";
        let at = |engine: &mut Engine, trends: &mut Trends, hour: i64, used: f64| {
            let now = start + hour * 3_600_000;
            let mut effects = Vec::new();
            engine.command(&format!("[{}] PROCESS_SERVICE_CHECK_RESULT;nas;disk;0;fine|/={used}MB;;;0;1000", now / 1000), now, &mut effects);
            let result = trends.evaluate(engine, line, now);
            match result.outcome {
                Outcome::Exited(code) => (code, result.stdout.trim().to_string()),
                other => panic!("{other:?}"),
            }
        };
        // Not checked yet, then too little watched: nothing is claimed.
        assert_eq!(trends.evaluate(&engine, line, start).stdout.trim(), "TREND OK - nas/disk is not checked yet");
        assert!(at(&mut engine, &mut trends, 0, 500.0).1.contains("too short to say"));
        // 10 MB an hour from 500 of 1000: 240 a day, full in two days and a bit.
        let mut last = (0, String::new());
        for hour in 1..=6 {
            last = at(&mut engine, &mut trends, hour, 500.0 + 10.0 * hour as f64);
        }
        assert_eq!(last.0, 2, "{}", last.1);
        assert!(last.1.starts_with("TREND CRITICAL - / is 560 of 1000, growing 240 a day: full in 44 hours|days_left=1.83;7;2"), "{}", last.1);
        // What was read is to be written, and a trend restored from it
        // goes on where it was: no half hour of "too short to say".
        let (added, trimmed) = trends.changes();
        assert_eq!(added.len(), 7);
        assert!(trimmed.is_empty());
        assert_eq!(trends.changes().0.len(), 0);
        let mut restored = Trends::restored(added);
        let now = start + 6 * 3_600_000;
        assert!(restored.evaluate(&engine, line, now).stdout.starts_with("TREND CRITICAL - / is 560 of 1000, growing 240 a day"));
        assert!(restored.changes().0.is_empty(), "the same result is not read twice");
        // It stops growing: the fitted line flattens as the old readings age.
        for hour in 7..=40 {
            last = at(&mut engine, &mut trends, hour, 560.0);
        }
        assert_eq!(last.0, 0, "{}", last.1);
        assert!(last.1.contains("not growing"), "{}", last.1);
        // Readings older than a day were dropped, and the store is told.
        let (_, trimmed) = trends.changes();
        assert_eq!(trimmed, vec![(line.to_string(), start + 16 * 3_600_000)]);
        // What cannot be worked out.
        for bad in ["checkd:trend nas/disk", "checkd:trend nas/disk inodes", "checkd:trend nowhere/disk /", "checkd:trend nas/disk / --warn soon"] {
            assert!(matches!(trends.evaluate(&engine, bad, start).outcome, Outcome::Exited(3)), "{bad}");
        }
    }

    #[test]
    fn numbers_as_a_person_reads_them() {
        assert_eq!(rounded(560.0), "560");
        assert_eq!(rounded(55.5), "55.5");
        assert_eq!(rounded(0.25), "0.25");
        assert_eq!(rounded(295_039_696_896.0), "295G");
        assert_eq!(rounded(12_500.0), "12.5k");
        assert_eq!(rounded(-3_400_000.0), "-3.40M");
    }

    #[test]
    fn how_fast_it_grows() {
        let hour = 3_600_000;
        let steady: VecDeque<(Ms, f64)> = (0..25).map(|h| (h * hour, 100.0 + h as f64)).collect();
        assert!((a_day(&steady).unwrap() - 24.0).abs() < 1e-6);
        let flat: VecDeque<(Ms, f64)> = (0..25).map(|h| (h * hour, 100.0)).collect();
        assert_eq!(a_day(&flat), Some(0.0));
        // A reading that jumps does not undo the rest.
        let noisy: VecDeque<(Ms, f64)> = (0..25).map(|h| (h * hour, 100.0 + h as f64 + if h == 12 { 5.0 } else { 0.0 })).collect();
        assert!((a_day(&noisy).unwrap() - 24.0).abs() < 1.0);
        assert_eq!(a_day(&VecDeque::from([(0, 1.0)])), None);
    }
}
