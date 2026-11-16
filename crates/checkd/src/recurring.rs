//! Downtimes that come round: a host or service with the custom variable
//! `_CHECKD_DOWNTIME_PERIOD <timeperiod>` is in a downtime whenever that
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
            let Some(period) = engine.item(obj).cfg.custom.iter().find(|(n, _)| n == "CHECKD_DOWNTIME_PERIOD").map(|(_, v)| v.trim().to_string()) else {
                continue;
            };
            if engine.objects().timeperiods.contains_key(&period) {
                wanted.push((obj, period));
            } else {
                notes.push(format!(
                    "Warning: {}: _CHECKD_DOWNTIME_PERIOD names no timeperiod '{period}'; no downtime is made",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{NOW, commands, engine};

    const HOUR: Ms = 3_600_000;

    /// Fridays 07:00 to 10:30 UTC for the host; every day, all day, for one
    /// service; and one that names nothing.
    const OBJECTS: &str = "define timeperiod {\n timeperiod_name backups\n alias backups\n friday 07:00-10:30\n}\n\
        define host {\n host_name nas\n address 192.0.2.1\n check_command ok\n max_check_attempts 1\n check_period always\n _CHECKD_DOWNTIME_PERIOD backups\n}\n\
        define service {\n host_name nas\n service_description scrub\n check_command ok\n max_check_attempts 1\n check_period always\n _CHECKD_DOWNTIME_PERIOD always\n}\n\
        define service {\n host_name nas\n service_description typo\n check_command ok\n max_check_attempts 1\n check_period always\n _CHECKD_DOWNTIME_PERIOD backup\n}\n";

    #[test]
    fn a_downtime_whenever_the_timeperiod_holds() {
        let mut engine = engine(OBJECTS, "");
        let (recurring, notes) = Recurring::new(&engine);
        assert!(recurring.any());
        assert_eq!(notes, ["Warning: nas;typo: _CHECKD_DOWNTIME_PERIOD names no timeperiod 'backup'; no downtime is made"]);

        // Friday 08:00: the host to 10:30; the service for seven days at most.
        let due = recurring.due(&engine, NOW);
        let start = NOW / 1000;
        assert_eq!(due.len(), 2, "{due:?}");
        assert!(due.contains(&format!("[{start}] SCHEDULE_HOST_DOWNTIME;nas;{start};{};1;0;0;checkd;recurring: backups", start + 2 * 3600 + 1800)));
        assert!(due.contains(&format!("[{start}] SCHEDULE_SVC_DOWNTIME;nas;scrub;{start};{};1;0;0;checkd;recurring: always", start + 7 * 86_400)));
        commands(&mut engine, NOW, &due);
        assert_eq!(engine.downtimes().filter(|d| d.author == AUTHOR).count(), 2);

        // Made once: a quarter of a minute later there is nothing to do.
        assert!(recurring.due(&engine, NOW + 15_000).is_empty());

        // Known again after a restart, by author and comment.
        let snapshot = engine.snapshot();
        let mut again = crate::testing::engine(OBJECTS, "");
        again.restore(&snapshot);
        let (recurring, _) = Recurring::new(&again);
        assert!(recurring.due(&again, NOW + HOUR).is_empty());

        // Taken away while the timeperiod holds: back at the next look.
        let id = again.downtimes().find(|d| matches!(d.obj, Obj::Host(_))).unwrap().id;
        commands(&mut again, NOW + HOUR, &[format!("[{}] DEL_HOST_DOWNTIME;{id}", (NOW + HOUR) / 1000)]);
        assert_eq!(recurring.due(&again, NOW + HOUR).len(), 1);

        // Outside the timeperiod: nothing for the host.
        let saturday = NOW + 24 * HOUR;
        let fresh = crate::testing::engine(OBJECTS, "");
        let due = Recurring::new(&fresh).0.due(&fresh, saturday);
        assert!(due.iter().all(|line| !line.contains("SCHEDULE_HOST_DOWNTIME")), "{due:?}");
    }
}
