//! Downtimes that come round: a host or service with the custom variable
//! `_DOWNTIME_PERIOD <timeperiod>` is in a downtime whenever that
//! timeperiod holds. See `docs/downtimes.md`.
//!
//! The downtimes are ordinary ones, made by the external commands for
//! them when the timeperiod begins to hold, and known again after a
//! restart by their author and comment.

use checkd_core::engine::{Engine, Ms, Obj};

pub const AUTHOR: &str = "checkd";

/// The longest a downtime is made for at a time. A timeperiod that holds
/// for longer gets another downtime when this one ends.
const LONGEST: i64 = 7 * 86_400;

fn comment(period: &str) -> String {
    format!("recurring: {period}")
}

pub struct Recurring {
    /// Each object that has the variable, and its timeperiod.
    wanted: Vec<(Obj, String)>,
}

impl Recurring {
    /// The second part is what to tell whoever starts checkd.
    pub fn new(engine: &Engine) -> (Recurring, Vec<String>) {
        let mut wanted = Vec::new();
        let mut notes = Vec::new();
        for obj in engine.all_objects() {
            let Some(period) = engine.item(obj).cfg.custom.iter().find(|(n, _)| n == "DOWNTIME_PERIOD").map(|(_, v)| v.trim().to_string()) else {
                continue;
            };
            if engine.objects().timeperiods.contains_key(&period) {
                wanted.push((obj, period));
            } else {
                notes.push(format!(
                    "Warning: {}: _DOWNTIME_PERIOD names no timeperiod '{period}'; no downtime is made",
                    engine.log_name(obj)
                ));
            }
        }
        (Recurring { wanted }, notes)
    }

    pub fn any(&self) -> bool {
        !self.wanted.is_empty()
    }

    /// The commands for the downtimes that are to begin now.
    pub fn due(&self, engine: &Engine, now: Ms) -> Vec<String> {
        let second = now.div_euclid(1000);
        let mut commands = Vec::new();
        for (obj, period) in &self.wanted {
            if !engine.in_period(Some(period), second) {
                continue;
            }
            let comment = comment(period);
            let there = engine
                .downtimes()
                .any(|d| d.obj == *obj && d.author == AUTHOR && d.comment == comment && d.start <= second && d.end > second);
            if there {
                continue;
            }
            // To the minute in which the timeperiod stops holding.
            let mut end = second - second.rem_euclid(60) + 60;
            while end - second < LONGEST && engine.in_period(Some(period), end) {
                end += 60;
            }
            let name = engine.log_name(*obj);
            let command = match obj {
                Obj::Host(_) => "SCHEDULE_HOST_DOWNTIME",
                Obj::Service(_) => "SCHEDULE_SVC_DOWNTIME",
            };
            commands.push(format!("[{second}] {command};{name};{second};{end};1;0;0;{AUTHOR};{comment}"));
        }
        commands
    }
}
