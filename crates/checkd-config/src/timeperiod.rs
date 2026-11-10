//! Day lines, date exceptions and time ranges of timeperiods.

use std::fmt;

pub const WEEKDAYS: [&str; 7] = [
    "sunday",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
];
pub const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// Part of a day, in seconds from midnight. The end can be 86400.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeRange {
    pub start: u32,
    pub end: u32,
}

impl fmt::Display for TimeRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (s, e) = (self.start / 60, self.end / 60);
        write!(f, "{:02}:{:02}-{:02}:{:02}", s / 60, s % 60, e / 60, e % 60)
    }
}

/// Reads a time of day the way the reference does. The flag says whether
/// it was written as plain `HH:MM`.
///
/// The reference takes the digits before the colon as hours and the digits
/// after it as minutes, drops whatever follows the minutes, and lets
/// minutes above 59 run into the next hours: `09:75` is 10:15. A time past
/// 24:00 is an error.
fn parse_time(text: &str) -> Result<(u32, bool), String> {
    let bad = || format!("'{text}' is not a time of day written HH:MM");
    let (hours, rest) = text.split_once(':').ok_or_else(bad)?;
    let length = rest.bytes().take_while(u8::is_ascii_digit).count();
    let minutes = &rest[..length];
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    if !digits(hours) || !digits(minutes) {
        return Err(bad());
    }
    let past = || format!("'{text}' is past the end of the day");
    // More digits than this is past the end of the day whatever they are.
    let number = |part: &str| -> Result<u32, String> {
        match part.trim_start_matches('0') {
            "" => Ok(0),
            part if part.len() <= 5 => part.parse().map_err(|_| bad()),
            _ => Err(past()),
        }
    };
    let (hours, minutes) = (number(hours)?, number(minutes)?);
    let seconds = hours * 3600 + minutes * 60;
    if seconds > 86400 {
        return Err(past());
    }
    Ok((seconds, minutes <= 59 && length == rest.len()))
}

/// Splits `HH:MM-HH:MM,HH:MM-HH:MM` into ranges as written, with whether
/// every time in it was plain `HH:MM`.
fn read_ranges(text: &str) -> Result<(Vec<TimeRange>, bool), String> {
    if text.is_empty() {
        return Err("no times are given".to_string());
    }
    let mut ranges = Vec::new();
    let mut plain = true;
    for part in text.split(',') {
        let (start, end) = part
            .split_once('-')
            .ok_or_else(|| format!("'{part}' is not a time range written HH:MM-HH:MM"))?;
        let (start, start_plain) = parse_time(start)?;
        let (end, end_plain) = parse_time(end)?;
        plain = plain && start_plain && end_plain;
        ranges.push(TimeRange { start, end });
    }
    Ok((ranges, plain))
}

/// Parses `HH:MM-HH:MM,HH:MM-HH:MM`. The ranges come back sorted by start.
pub fn parse_ranges(text: &str) -> Result<Vec<TimeRange>, String> {
    let (mut ranges, _) = read_ranges(text)?;
    ranges.sort_by_key(|range| range.start);
    Ok(ranges)
}

/// A warning for times that are taken, but not as written.
pub fn times_warning(text: &str) -> Option<String> {
    match read_ranges(text) {
        Ok((ranges, false)) => {
            let taken: Vec<String> = ranges.iter().map(TimeRange::to_string).collect();
            Some(format!(
                "'{text}' is not written HH:MM-HH:MM; Naemon reads it as {}, and checkd does the same",
                taken.join(",")
            ))
        }
        _ => None,
    }
}

/// One end of a date exception.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Day {
    /// `2026-12-25`
    Calendar { year: u32, month: u32, day: u32 },
    /// `february 10`, or `february -1` for the last day of February
    MonthDate { month: usize, day: i32 },
    /// `day 2`, or `day -1` for the last day of any month
    MonthDay { day: i32 },
    /// `thursday -1 november`
    WeekdayOfMonth {
        weekday: usize,
        nth: i32,
        month: usize,
    },
    /// `monday 3`, the third Monday of any month
    Weekday { weekday: usize, nth: i32 },
}

impl Day {
    /// Where exceptions of this form come in the resolved-object dump.
    pub fn rank(&self) -> u8 {
        match self {
            Day::Calendar { .. } => 0,
            Day::MonthDate { .. } => 1,
            Day::MonthDay { .. } => 2,
            Day::WeekdayOfMonth { .. } => 3,
            Day::Weekday { .. } => 4,
        }
    }
}

impl fmt::Display for Day {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Day::Calendar { year, month, day } => write!(f, "{year:04}-{month:02}-{day:02}"),
            Day::MonthDate { month, day } => write!(f, "{} {day}", MONTHS[month]),
            Day::MonthDay { day } => write!(f, "day {day}"),
            Day::WeekdayOfMonth {
                weekday,
                nth,
                month,
            } => {
                write!(f, "{} {nth} {}", WEEKDAYS[weekday], MONTHS[month])
            }
            Day::Weekday { weekday, nth } => write!(f, "{} {nth}", WEEKDAYS[weekday]),
        }
    }
}

/// A date line of a timeperiod: a day or a span of days, and the times
/// that are valid on them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exception {
    pub start: Day,
    /// `None` for a single day.
    pub end: Option<Day>,
    /// Every how many days within the span, when given with `/ N`.
    pub skip: Option<u32>,
    pub ranges: Vec<TimeRange>,
}

impl Exception {
    /// The days part, as the resolved-object dump writes it.
    pub fn days(&self) -> String {
        let mut text = self.start.to_string();
        match self.end {
            Some(end) if end != self.start => match end {
                Day::MonthDay { day } => text.push_str(&format!(" - {day}")),
                _ => text.push_str(&format!(" - {end}")),
            },
            _ => {}
        }
        if let Some(skip) = self.skip {
            text.push_str(&format!(" / {skip}"));
        }
        text
    }
}

/// A parsed day or date line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// A weekday, 0 for Sunday.
    Weekday(usize, Vec<TimeRange>),
    Exception(Exception),
}

fn position(names: &[&str], word: &str) -> Option<usize> {
    names.iter().position(|name| *name == word)
}

fn parse_offset(word: &str, limit: i32) -> Option<i32> {
    word.parse::<i32>()
        .ok()
        .filter(|n| *n != 0 && n.abs() <= limit)
}

fn parse_calendar(word: &str) -> Option<Day> {
    let parts: Vec<&str> = word.split('-').collect();
    let [year, month, day] = parts.as_slice() else {
        return None;
    };
    if year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }
    let (year, month, day) = (year.parse().ok()?, month.parse().ok()?, day.parse().ok()?);
    ((1..=12).contains(&month) && (1..=31).contains(&day)).then_some(Day::Calendar {
        year,
        month,
        day,
    })
}

/// Parses one end of a span. `start` is given for the second end, which
/// may leave out what it shares with the first: `july 10 - 15`,
/// `day 1 - 15`, `monday 1 - 3`.
fn parse_day(words: &[&str], start: Option<&Day>) -> Option<Day> {
    match (words, start) {
        ([word], Some(Day::MonthDate { month, .. })) if parse_offset(word, 31).is_some() => {
            Some(Day::MonthDate {
                month: *month,
                day: parse_offset(word, 31)?,
            })
        }
        ([word], Some(Day::MonthDay { .. })) if parse_offset(word, 31).is_some() => {
            Some(Day::MonthDay {
                day: parse_offset(word, 31)?,
            })
        }
        ([word], Some(Day::Weekday { weekday, .. })) if parse_offset(word, 5).is_some() => {
            Some(Day::Weekday {
                weekday: *weekday,
                nth: parse_offset(word, 5)?,
            })
        }
        ([word], _) => parse_calendar(word),
        (["day", n], _) => Some(Day::MonthDay {
            day: parse_offset(n, 31)?,
        }),
        ([name, n], _) => match (position(&MONTHS, name), position(&WEEKDAYS, name)) {
            (Some(month), _) => Some(Day::MonthDate {
                month,
                day: parse_offset(n, 31)?,
            }),
            (_, Some(weekday)) => Some(Day::Weekday {
                weekday,
                nth: parse_offset(n, 5)?,
            }),
            _ => None,
        },
        ([name, n, month], _) => Some(Day::WeekdayOfMonth {
            weekday: position(&WEEKDAYS, name)?,
            nth: parse_offset(n, 5)?,
            month: position(&MONTHS, month)?,
        }),
        _ => None,
    }
}

/// Parses a day or date line. `days` is the line without its last word,
/// and `times` is that last word.
pub fn parse_entry(days: &str, times: &str) -> Result<Entry, String> {
    let ranges = parse_ranges(times).map_err(|why| format!("'{days} {times}': {why}"))?;
    if let Some(weekday) = position(&WEEKDAYS, days) {
        return Ok(Entry::Weekday(weekday, ranges));
    }
    let bad = || format!("'{days}' is not a setting of a timeperiod, a weekday or a date");
    let mut words: Vec<&str> = days.split_whitespace().collect();
    let mut skip = None;
    if let Some(at) = words.iter().position(|word| *word == "/") {
        let [number] = &words[at + 1..] else {
            return Err(bad());
        };
        skip = Some(
            number
                .parse::<u32>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(bad)?,
        );
        words.truncate(at);
    }
    let (first, second) = match words.iter().position(|word| *word == "-") {
        Some(at) => (&words[..at], Some(&words[at + 1..])),
        None => (&words[..], None),
    };
    let start = parse_day(first, None).ok_or_else(bad)?;
    let end = match second {
        Some(words) => {
            let end = parse_day(words, Some(&start)).ok_or_else(bad)?;
            if end.rank() != start.rank() {
                return Err(format!(
                    "'{days}': the two ends of the span are not the same kind of date"
                ));
            }
            Some(end)
        }
        None => None,
    };
    Ok(Entry::Exception(Exception {
        start,
        end,
        skip,
        ranges,
    }))
}
