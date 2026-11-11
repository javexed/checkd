//! Flap detection: how much an object's state changed over its last
//! results.
//!
//! The reference keeps the last 20 recorded states. When a new result is
//! to be recorded it first works out the percent change over the 20 states
//! it already has, and only then adds the new one. So a change shows in
//! the number one result late.
//!
//! Between 20 states there are 19 places where a change can be. A change
//! at the newest place weighs 1.25, each older place 0.5/19 less, down to
//! 0.776 at the oldest. The sum is divided by 20 and given as a percentage,
//! so one change counts between 3.88 % and 6.25 %.

/// How many states are kept.
pub const LEN: usize = 20;

/// The last recorded states of one host or service, oldest first.
#[derive(Clone, Debug)]
pub struct History {
    states: [u8; LEN],
}

/// What a new result means for flapping.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Change {
    None,
    Started,
    Stopped,
}

impl History {
    /// A history as it is at start: the initial state all the way.
    pub fn new(state: u8) -> History {
        History {
            states: [state; LEN],
        }
    }

    /// The percent change over the states kept now.
    pub fn percent_change(&self) -> f64 {
        let mut sum = 0.0;
        for place in 1..LEN {
            if self.states[place] != self.states[place - 1] {
                sum += 0.75 + place as f64 * 0.5 / (LEN - 1) as f64;
            }
        }
        sum / LEN as f64 * 100.0
    }

    /// Works out the percent change, then adds the state.
    pub fn record(&mut self, state: u8) -> f64 {
        let percent = self.percent_change();
        self.states.rotate_left(1);
        self.states[LEN - 1] = state;
        percent
    }
}

/// Whether flapping starts or stops at this percent change.
///
/// It starts at the high threshold or above and stops below the low one.
pub fn judge(flapping: bool, percent: f64, low: f64, high: f64) -> Change {
    if !flapping && percent >= high {
        Change::Started
    } else if flapping && percent < low {
        Change::Stopped
    } else {
        Change::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two(value: f64) -> String {
        format!("{value:.2}")
    }

    /// A service that goes between OK and CRITICAL fourteen times and then
    /// stays OK. The numbers are the reference's `$SERVICEPERCENTCHANGE$`
    /// after each result.
    #[test]
    fn alternating_series() {
        let mut history = History::new(0);
        let mut seen = Vec::new();
        for n in 0..17 {
            let state = if n < 14 && n % 2 == 0 { 2 } else { 0 };
            seen.push(two(history.record(state)));
        }
        assert_eq!(
            seen,
            [
                "0.00", "6.25", "12.37", "18.36", "24.21", "29.93", "35.53", "40.99", "46.32",
                "51.51", "56.58", "61.51", "66.32", "70.99", "75.53", "73.68", "71.84"
            ]
        );
    }

    #[test]
    fn one_change_fades_out() {
        let mut history = History::new(0);
        history.record(1);
        let mut seen = Vec::new();
        for _ in 0..21 {
            seen.push(two(history.record(1)));
        }
        assert_eq!(seen[0], "6.25");
        assert_eq!(seen[1], "6.12");
        assert_eq!(seen[2], "5.99");
        assert_eq!(seen[18], "3.88");
        assert_eq!(seen[19], "0.00");
    }

    #[test]
    fn two_changes() {
        // Down for four results, then up: 6.25 for the newest change and
        // 5.72 for the one four places back.
        let mut history = History::new(0);
        for state in [1, 1, 1, 1, 0] {
            history.record(state);
        }
        assert_eq!(two(history.percent_change()), "11.97");
    }

    #[test]
    fn thresholds() {
        assert_eq!(judge(false, 30.0, 20.0, 30.0), Change::Started);
        assert_eq!(judge(false, 29.9, 20.0, 30.0), Change::None);
        assert_eq!(judge(true, 20.0, 20.0, 30.0), Change::None);
        assert_eq!(judge(true, 19.9, 20.0, 30.0), Change::Stopped);
    }
}
