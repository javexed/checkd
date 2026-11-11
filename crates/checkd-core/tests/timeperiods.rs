//! The evaluator against the reference's answers in `runs/timeperiods`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use checkd_config::model::Objects;
use checkd_core::timeperiod::{days_from_civil, Fixed, Periods};

/// Periods for which checkd goes by the documentation where the
/// reference does something else. `runs/timeperiods/README.md` has the
/// details.
const ON_PURPOSE: &[(&str, &str)] = &[
    ("cal-span-skip", "the reference takes no notice of the end of a span with a skip"),
    ("day-span-skip", "the same"),
    ("md-span-skip", "the same"),
    ("wd-span-skip", "the same"),
    ("wom-span-skip", "the same"),
    ("day-last", "the reference counts from the 31st in every month"),
    ("day-negative-span", "the same"),
    ("md-last", "the same"),
    ("md-span-wrap", "the reference ends the span with the year"),
    ("md-span-past-end", "the reference runs the span to the end of the year"),
    ("nothing-left", "the reference names a next time at which the period is not valid"),
];

/// The reference stops looking somewhere near 300 days ahead, checkd
/// after `HORIZON_DAYS`. An answer of checkd's beyond this many days
/// against a `never` of the reference is that difference.
const REFERENCE_LOOKS_AHEAD: i64 = 289 * 86400;

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runs/timeperiods")
}

fn objects() -> Objects {
    let loaded = checkd_config::load(&dir().join("naemon.cfg"), &Default::default());
    loaded.objects.expect("the periods load")
}

#[test]
fn same_answers_as_the_reference() {
    let objects = objects();
    let zone = Fixed(0);
    let periods = Periods::new(&objects.timeperiods, &zone);

    let table = std::fs::read_to_string(dir().join("expected.tsv")).unwrap();
    let mut wrong: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut asked = 0;
    let mut beyond = 0;
    for line in table.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let [name, moment, valid, next] = fields[..] else {
            panic!("bad line {line}");
        };
        let moment: i64 = moment.parse().unwrap();
        let valid = valid == "1";
        let next = (next != "never").then(|| moment + next.parse::<i64>().unwrap());
        asked += 1;
        let got = (periods.is_valid(name, moment), periods.next_valid(name, moment));
        if got == (valid, next) {
            continue;
        }
        if !valid && !got.0 && next.is_none() && got.1.is_some_and(|at| at - moment > REFERENCE_LOOKS_AHEAD) {
            beyond += 1;
            continue;
        }
        wrong.entry(name).or_default().push(format!(
            "{name} at {moment}: valid {valid}, next {next:?}; here valid {}, next {:?}",
            got.0, got.1
        ));
    }
    assert!(asked > 20000);
    assert!(beyond > 0, "the reference looks as far ahead as checkd now");

    let on_purpose: BTreeSet<&str> = ON_PURPOSE.iter().map(|(name, _)| *name).collect();
    for name in &on_purpose {
        assert!(wrong.contains_key(name), "{name} is the same as the reference now");
    }
    wrong.retain(|name, _| !on_purpose.contains(name));
    if !wrong.is_empty() {
        let show = std::env::var("SHOW").map_or(3, |n| n.parse().unwrap());
        for (name, lines) in &wrong {
            eprintln!("{name}: {} differ", lines.len());
            for line in lines.iter().take(show) {
                eprintln!("  {line}");
            }
        }
        panic!("{} periods differ", wrong.len());
    }
}

/// A moment written as `2027-03-08 09:00:00`.
fn at(text: &str) -> i64 {
    let numbers: Vec<i64> = text
        .split(|c: char| !c.is_ascii_digit())
        .map(|part| part.parse().unwrap())
        .collect();
    let [year, month, day, hour, minute, second] = numbers[..] else {
        panic!("bad moment {text}");
    };
    days_from_civil(year, month, day) * 86400 + hour * 3600 + minute * 60 + second
}

/// What checkd answers where it differs from the reference on purpose.
#[test]
fn by_the_documentation() {
    let objects = objects();
    let zone = Fixed(0);
    let periods = Periods::new(&objects.timeperiods, &zone);
    let valid = |name: &str, moment: &str| periods.is_valid(name, at(moment));
    let next = |name: &str, moment: &str| periods.next_valid(name, at(moment));
    let some = |moment: &str| Some(at(moment));

    // A span with a skip ends where it says.
    // 2027-03-01 - 2027-04-30 / 3
    assert!(valid("cal-span-skip", "2027-04-30 12:00:00"));
    assert!(!valid("cal-span-skip", "2027-05-06 12:00:00"));
    assert_eq!(next("cal-span-skip", "2027-05-01 00:00:00"), None);
    // day 1 - 15 / 5
    assert!(valid("day-span-skip", "2027-01-11 12:00:00"));
    assert!(!valid("day-span-skip", "2027-01-16 12:00:00"));
    assert_eq!(next("day-span-skip", "2027-01-12 00:00:00"), some("2027-02-01 00:00:00"));
    // april 1 - june 30 / 10
    assert!(valid("md-span-skip", "2027-06-30 12:00:00"));
    assert!(!valid("md-span-skip", "2027-07-10 12:00:00"));
    // monday 1 - friday 3 / 2, in February 2027: the 1st to the 19th
    assert!(valid("wd-span-skip", "2027-02-19 12:00:00"));
    assert!(!valid("wd-span-skip", "2027-02-25 12:00:00"));
    // monday 1 march - sunday -1 march / 2, in 2027: the 1st to the 28th
    assert!(valid("wom-span-skip", "2027-03-27 12:00:00"));
    assert!(!valid("wom-span-skip", "2027-03-29 12:00:00"));
    assert!(!valid("wom-span-skip", "2027-04-06 12:00:00"));

    // A negative day counts from the month's real end.
    // day -1
    assert!(valid("day-last", "2027-02-28 12:00:00"));
    assert!(valid("day-last", "2027-04-30 12:00:00"));
    assert!(valid("day-last", "2028-02-29 12:00:00"));
    assert!(!valid("day-last", "2028-02-28 12:00:00"));
    // day -5 - -1
    assert_eq!(next("day-negative-span", "2027-02-01 00:00:00"), some("2027-02-24 00:00:00"));
    assert_eq!(next("day-negative-span", "2027-04-01 00:00:00"), some("2027-04-26 00:00:00"));
    // february -1
    assert!(valid("md-last", "2027-02-28 12:00:00"));
    assert!(!valid("md-last", "2028-02-28 12:00:00"));
    assert!(valid("md-last", "2028-02-29 12:00:00"));

    // december 20 - january 5 goes on after the turn of the year.
    assert!(valid("md-span-wrap", "2027-12-31 23:59:59"));
    assert!(valid("md-span-wrap", "2028-01-05 12:00:00"));
    assert!(!valid("md-span-wrap", "2028-01-06 00:00:00"));
    assert!(valid("md-span-wrap", "2027-01-01 00:00:00"));

    // february 20 - 30 ends with February.
    assert!(valid("md-span-past-end", "2027-02-28 12:00:00"));
    assert!(!valid("md-span-past-end", "2027-03-01 00:00:00"));
    assert!(valid("md-span-past-end", "2028-02-29 12:00:00"));

    // Office hours with every time excluded have no next time.
    assert_eq!(next("nothing-left", "2027-01-01 00:00:00"), None);

    // A year ahead is found, more is not.
    // march 17 09:00-17:00
    assert_eq!(next("md-single", "2027-03-17 17:00:00"), some("2028-03-17 09:00:00"));
    // 2029-06-01
    assert_eq!(next("cal-far", "2028-03-01 00:00:00"), None);
    assert_eq!(next("cal-far", "2028-06-01 00:00:00"), some("2029-06-01 00:00:00"));
}

/// The periods that differ on purpose are the ones a warning is given
/// for at the start, but for the one that only its exclusions empty.
#[test]
fn warned_about() {
    let objects = objects();
    let noted: BTreeSet<&str> = objects
        .timeperiods
        .values()
        .filter(|period| !checkd_core::timeperiod::notes(period).is_empty())
        .map(|period| period.name.as_str())
        .collect();
    let on_purpose: BTreeSet<&str> = ON_PURPOSE
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| *name != "nothing-left")
        .collect();
    assert_eq!(noted, on_purpose);
}
