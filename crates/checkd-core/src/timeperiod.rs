//! When a timeperiod is valid.
//!
//! A period is worked out one local day at a time. The day's times are:
//!
//! 1. the times of the one line that names the day most exactly. A
//!    calendar date beats a date of a month (`march 8`), which beats a
//!    day of any month (`day 8`), which beats a weekday of a month
//!    (`monday 2 march`), which beats a weekday of any month
//!    (`monday 2`), which beats a plain weekday. Of two lines of one
//!    kind the one written first counts, and the other adds nothing;
//! 2. less the times at which a period named in `exclude` is valid.
//!
//! A time range holds its first second and not its last: `09:00-17:00`
//! is valid at 09:00:00 and 16:59:59, not at 17:00:00.

use std::collections::BTreeMap;

use checkd_config::model::Timeperiod;
use checkd_config::timeperiod::{Day, Exception};

const DAY: i64 = 86400;

/// How many days ahead the next valid time is looked for.
pub const HORIZON_DAYS: i64 = 366;

/// Local time. The engine counts in seconds since the epoch, and a
/// period is written in local time.
pub trait Zone {
    /// How many seconds local time is ahead of UTC at this moment.
    fn offset(&self, time: i64) -> i64;

    /// What the zone is called at this moment, as in `UTC` or `+0200`.
    fn name(&self, time: i64) -> String {
        let offset = self.offset(time);
        if offset == 0 {
            return "UTC".to_string();
        }
        let sign = if offset < 0 { '-' } else { '+' };
        let minutes = offset.abs() / 60;
        format!("{sign}{:02}{:02}", minutes / 60, minutes % 60)
    }
}

/// UTC, or any zone that stays a fixed number of seconds ahead of it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Fixed(pub i64);

impl Zone for Fixed {
    fn offset(&self, _time: i64) -> i64 {
        self.0
    }
}

/// The periods of a configuration.
pub struct Periods<'a> {
    periods: &'a BTreeMap<String, Timeperiod>,
    zone: &'a dyn Zone,
}

/// Times of one day, in seconds from midnight: sorted, apart, each
/// `(start, end)` with the end left out.
type Spans = Vec<(i64, i64)>;

impl<'a> Periods<'a> {
    pub fn new(periods: &'a BTreeMap<String, Timeperiod>, zone: &'a dyn Zone) -> Periods<'a> {
        Periods { periods, zone }
    }

    /// Whether the period is valid at `time`. A period that is not
    /// defined is never valid.
    pub fn is_valid(&self, name: &str, time: i64) -> bool {
        let local = time + self.zone.offset(time);
        let second = local.rem_euclid(DAY);
        self.spans(name, local.div_euclid(DAY), &mut Vec::new())
            .iter()
            .any(|&(start, end)| start <= second && second < end)
    }

    /// The first moment at or after `time` at which the period is valid,
    /// if there is one in the days looked at.
    pub fn next_valid(&self, name: &str, time: i64) -> Option<i64> {
        let offset = self.zone.offset(time);
        let local = time + offset;
        let first = local.div_euclid(DAY);
        for day in first..=first + HORIZON_DAYS {
            let from = if day == first { local.rem_euclid(DAY) } else { 0 };
            for (start, end) in self.spans(name, day, &mut Vec::new()) {
                if from < end {
                    return Some(day * DAY + start.max(from) - offset);
                }
            }
        }
        None
    }

    /// The valid times of one day. `above` holds the periods this one is
    /// being worked out for, so that periods that exclude each other in a
    /// circle come to an end.
    fn spans(&self, name: &str, day: i64, above: &mut Vec<&'a str>) -> Spans {
        let Some(period) = self.periods.get(name) else {
            return Spans::new();
        };
        if above.contains(&period.name.as_str()) {
            return Spans::new();
        }
        let mut spans = own_spans(period, day);
        if spans.is_empty() || period.exclude.is_empty() {
            return spans;
        }
        above.push(&period.name);
        for excluded in &period.exclude {
            let without = self.spans(excluded, day, above);
            spans = subtract(spans, &without);
        }
        above.pop();
        spans
    }
}

/// The lines of a period that checkd reads by the documentation where
/// the reference does something else: one sentence for each, for the
/// warnings at the start. `runs/timeperiods/README.md` has the list.
///
/// A period from which the excluded periods take everything away is on
/// that list too, and is not found here.
pub fn notes(period: &Timeperiod) -> Vec<String> {
    let mut notes = Vec::new();
    for exception in &period.exceptions {
        let mut note = |what: &str| {
            notes.push(format!(
                "timeperiod '{}', line '{}': {what}",
                period.name,
                exception.days()
            ));
        };
        let ends = [Some(exception.start), exception.end];
        if exception.end.is_some() && exception.skip.is_some_and(|skip| skip > 1) {
            note(
                "the days end where the span ends. The reference version goes on with them \
                 past the end of the span",
            );
        }
        let from_the_end = ends.iter().flatten().any(|day| match *day {
            Day::MonthDay { day } => day < 0,
            Day::MonthDate { month, day } => day < 0 && SHORTEST[month] < 31,
            _ => false,
        });
        if from_the_end {
            note(
                "a day below zero is counted from the last day of the month. The reference \
                 version counts from the 31st whatever the month",
            );
        }
        if let (Day::MonthDate { month, day }, Some(Day::MonthDate { month: to, day: last })) =
            (exception.start, exception.end)
        {
            if to < month || (to == month && day > 0 && last > 0 && last < day) {
                note(
                    "the span runs over the turn of the year. The reference version ends it \
                     on 31 December",
                );
            }
            if last > SHORTEST[to] {
                note(
                    "the span ends with its month where the month is shorter. The reference \
                     version runs it to the end of the year",
                );
            }
        }
    }
    notes
}

/// The length of each month in a year that is not a leap year.
const SHORTEST: [i32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// The times of a day by the period's own lines, exclusions aside.
fn own_spans(period: &Timeperiod, day: i64) -> Spans {
    // Of the lines that name the day, the one of the most exact kind
    // counts, and of two of one kind the one written first.
    let mut best: Option<&Exception> = None;
    for exception in &period.exceptions {
        let better = best.is_none_or(|best| exception.start.rank() < best.start.rank());
        if better && fits(exception, day) {
            best = Some(exception);
        }
    }
    let ranges = match best {
        Some(exception) => &exception.ranges,
        None => &period.weekdays[weekday(day)],
    };
    let mut spans: Spans = ranges
        .iter()
        .map(|range| (i64::from(range.start), i64::from(range.end)))
        .filter(|(start, end)| start < end)
        .collect();
    spans.sort();
    let mut joined = Spans::new();
    for (start, end) in spans {
        match joined.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => joined.push((start, end)),
        }
    }
    joined
}

fn subtract(spans: Spans, without: &Spans) -> Spans {
    let mut left = Spans::new();
    for (mut start, end) in spans {
        for &(from, to) in without {
            if to <= start || end <= from {
                continue;
            }
            if start < from {
                left.push((start, from));
            }
            start = to;
        }
        if start < end {
            left.push((start, end));
        }
    }
    left
}

/// Whether an exception line names this day.
///
/// A line that names no year is worked out for the day's own year, and
/// one that names no month for the day's own month. Where its end then
/// comes before its start, as in `day 25 - 5` or `december 20 - january
/// 5`, the span runs over the turn of the month or year: the days from
/// the start on fit, and so do the days up to the end.
fn fits(exception: &Exception, day: i64) -> bool {
    let (year, month, _) = civil(day);
    let calendar = matches!(exception.start, Day::Calendar { .. });
    let Some(first) = resolve(&exception.start, year, month, false) else {
        return false;
    };
    // The day the span began on, if the day is in it.
    let began = match &exception.end {
        // A calendar date with a skip and no end goes on for good.
        None if calendar && exception.skip.is_some() => (day >= first).then_some(first),
        None => (day == first).then_some(first),
        Some(end) => {
            let Some(last) = resolve(end, year, month, true) else {
                return false;
            };
            if first <= last {
                (first <= day && day <= last).then_some(first)
            } else if calendar {
                None
            } else if day >= first {
                Some(first)
            } else if day <= last {
                let (year, month) = match exception.start {
                    Day::MonthDay { .. } | Day::Weekday { .. } if month > 1 => (year, month - 1),
                    Day::MonthDay { .. } | Day::Weekday { .. } => (year - 1, 12),
                    _ => (year - 1, month),
                };
                resolve(&exception.start, year, month, false)
            } else {
                None
            }
        }
    };
    match (began, exception.skip) {
        (None, _) => false,
        (Some(began), Some(skip)) if skip > 1 => (day - began) % i64::from(skip) == 0,
        (Some(_), _) => true,
    }
}

/// The day a line's start or end stands for in a year and, for the kinds
/// that name no month, a month. `None` when the month has no such day.
/// The end of a span is the month's last day in that case.
fn resolve(day: &Day, year: i64, month: i64, end: bool) -> Option<i64> {
    let (month, found) = match *day {
        Day::Calendar { year, month, day } => {
            return Some(days_from_civil(i64::from(year), i64::from(month), i64::from(day)));
        }
        Day::MonthDate { month, day } => {
            let month = month as i64 + 1;
            (month, day_of_month(year, month, i64::from(day)))
        }
        Day::MonthDay { day } => (month, day_of_month(year, month, i64::from(day))),
        Day::WeekdayOfMonth { weekday, nth, month } => {
            let month = month as i64 + 1;
            (month, weekday_of_month(year, month, weekday, i64::from(nth)))
        }
        Day::Weekday { weekday, nth } => {
            (month, weekday_of_month(year, month, weekday, i64::from(nth)))
        }
    };
    let positive = match *day {
        Day::MonthDate { day, .. } | Day::MonthDay { day } => day > 0,
        Day::WeekdayOfMonth { nth, .. } | Day::Weekday { nth, .. } => nth > 0,
        Day::Calendar { .. } => false,
    };
    if found.is_none() && end && positive {
        return Some(days_from_civil(year, month, month_length(year, month)));
    }
    found
}

/// Day `day` of a month, counted from its end when negative.
fn day_of_month(year: i64, month: i64, day: i64) -> Option<i64> {
    let length = month_length(year, month);
    let day = if day < 0 { length + 1 + day } else { day };
    (1..=length)
        .contains(&day)
        .then(|| days_from_civil(year, month, day))
}

/// The `nth` such weekday of a month, counted from its end when negative.
fn weekday_of_month(year: i64, month: i64, wanted: usize, nth: i64) -> Option<i64> {
    let length = month_length(year, month);
    let first = days_from_civil(year, month, 1);
    let day = if nth > 0 {
        let to_first = (wanted as i64 - weekday(first) as i64).rem_euclid(7);
        1 + to_first + (nth - 1) * 7
    } else {
        let last = first + length - 1;
        let back = (weekday(last) as i64 - wanted as i64).rem_euclid(7);
        length - back + (nth + 1) * 7
    };
    (1..=length).contains(&day).then(|| first + day - 1)
}

fn month_length(year: i64, month: i64) -> i64 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 31,
    }
}

/// The weekday of a day counted from the epoch, Sunday being 0.
fn weekday(day: i64) -> usize {
    (day + 4).rem_euclid(7) as usize
}

/// Days from 1970-01-01 to a date of the Gregorian calendar.
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year.rem_euclid(400);
    let of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    era * 146097 + of_era * 365 + of_era / 4 - of_era / 100 + of_year - 719468
}

/// The year, month (1 to 12) and day of a day counted from 1970-01-01.
pub fn civil(day: i64) -> (i64, i64, i64) {
    let shifted = day + 719468;
    let era = shifted.div_euclid(146097);
    let of_era = shifted.rem_euclid(146097);
    let year_of_era = (of_era - of_era / 1460 + of_era / 36524 - of_era / 146096) / 365;
    let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * of_year + 2) / 153;
    let day = of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil(0), (1970, 1, 1));
        // 2027-03-08 is a Monday.
        let day = days_from_civil(2027, 3, 8);
        assert_eq!(civil(day), (2027, 3, 8));
        assert_eq!(weekday(day), 1);
        assert_eq!(civil(days_from_civil(2028, 2, 29)), (2028, 2, 29));
        assert_eq!(civil(days_from_civil(2028, 2, 29) + 1), (2028, 3, 1));
    }

    #[test]
    fn weekdays_of_a_month() {
        let date = |y, m, d| Some(days_from_civil(y, m, d));
        // March 2027 begins on a Monday and ends on a Wednesday.
        assert_eq!(weekday_of_month(2027, 3, 1, 1), date(2027, 3, 1));
        assert_eq!(weekday_of_month(2027, 3, 1, 5), date(2027, 3, 29));
        assert_eq!(weekday_of_month(2027, 3, 4, 5), None);
        assert_eq!(weekday_of_month(2027, 3, 3, -1), date(2027, 3, 31));
        assert_eq!(weekday_of_month(2027, 3, 0, -1), date(2027, 3, 28));
        assert_eq!(weekday_of_month(2027, 3, 4, -5), None);
        assert_eq!(weekday_of_month(2027, 11, 4, -1), date(2027, 11, 25));
    }
}
