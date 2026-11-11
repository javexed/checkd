//! Results that come from outside, and performance data.
//!
//! Passive results come as external commands (`PROCESS_SERVICE_CHECK_RESULT`,
//! `PROCESS_HOST_CHECK_RESULT`) or as files in the check result directory,
//! where NRDP and the like drop them. What the reference does with them,
//! from its log on configs written for the purpose:
//!
//! - `PASSIVE SERVICE CHECK: host;service;code;output` (or `PASSIVE HOST
//!   CHECK`) is logged first, the output without its performance data;
//! - a service goes through soft and hard states as for an active check; a
//!   code above 3 is UNKNOWN;
//! - a host goes to its hard state at once, unless
//!   `passive_host_checks_are_soft`; its code is the state, 0 to 2, and a
//!   higher one makes the command fail;
//! - a result for an object whose passive checks are off makes the command
//!   fail;
//! - the latency of a passive result is the time since the moment the
//!   command gives, and its execution time 0.
//!
//! Performance data: with `process_performance_data` on, every result of
//! an object whose own `process_perf_data` is on gives a line, by the
//! host or service template, for the performance data file. The driver
//! writes the files; the processing commands run on their intervals.

use crate::engine::{Effect, Engine, JobKind, Ms, Obj};
use crate::notify::Timer;
use crate::output;

impl Engine {
    /// A passive result. False when it is not taken.
    pub(crate) fn passive_result(
        &mut self,
        obj: Obj,
        code: i64,
        text: &str,
        checked_at: Ms,
        now: Ms,
        out: &mut Vec<Effect>,
    ) -> bool {
        let accepted = match obj {
            Obj::Host(_) => self.settings.accept_passive_host_checks && (0..=2).contains(&code),
            Obj::Service(_) => self.settings.accept_passive_service_checks,
        };
        if !accepted || !self.item(obj).cfg.passive_checks_enabled {
            return false;
        }
        // A literal `\n` in the text is a new line, as the reference reads
        // passive output.
        let output = output::parse(&text.replace("\\n", "\n"));
        let state = code.clamp(0, 3) as u8;
        if self.settings.log_passive_checks {
            let label = match obj {
                Obj::Host(_) => "PASSIVE HOST CHECK",
                Obj::Service(_) => "PASSIVE SERVICE CHECK",
            };
            out.push(Effect::Log(format!("{label}: {};{state};{}", self.log_name(obj), output.short)));
        }
        let item = self.item_mut(obj);
        item.latency = ((now - checked_at).max(0)) as f64 / 1000.0;
        item.passive_result = true;
        match obj {
            Obj::Service(at) => self.service_result(at, state, output, now, now, out),
            Obj::Host(at) => self.host_state(at, state, true, output, now, now, out),
        }
        self.item_mut(obj).execution_time = 0.0;
        true
    }

    /// A file of the check result directory, in the format NRDP and the
    /// like write: `key=value` lines with `host_name`, `service_description`
    /// for a service, `check_type` (0 active, 1 passive), `return_code`,
    /// `output`, `start_time` and `latency`. False when it names no object
    /// or is not taken.
    pub fn check_result_file(&mut self, text: &str, now: Ms, out: &mut Vec<Effect>) -> bool {
        let get = |key: &str| {
            text.lines()
                .find_map(|line| line.strip_prefix(key).and_then(|rest| rest.strip_prefix('=')))
                .map(str::to_string)
        };
        let Some(host) = get("host_name") else { return false };
        let obj = match get("service_description") {
            Some(service) => self.service(&host, &service),
            None => self.host(&host),
        };
        let Some(obj) = obj else { return false };
        let code: i64 = get("return_code").and_then(|c| c.trim().parse().ok()).unwrap_or(3);
        let output = get("output").unwrap_or_default();
        let latency: f64 = get("latency").and_then(|l| l.trim().parse().ok()).unwrap_or(0.0);
        let checked_at = now - (latency * 1000.0) as Ms;
        if get("check_type").as_deref().map(str::trim) == Some("0") {
            // An active result from elsewhere: no passive line in the log.
            let saved = self.settings.log_passive_checks;
            self.settings.log_passive_checks = false;
            let taken = self.passive_result(obj, code, &output, checked_at, now, out);
            self.settings.log_passive_checks = saved;
            taken
        } else {
            self.passive_result(obj, code, &output, checked_at, now, out)
        }
    }

    /// The line for the performance data file after a result, if one is
    /// written.
    pub(crate) fn perfdata(&mut self, obj: Obj, now: Ms, out: &mut Vec<Effect>) {
        if !self.settings.process_performance_data || !self.item(obj).cfg.process_perf_data {
            return;
        }
        let place = matches!(obj, Obj::Service(_)) as usize;
        let settings = &self.settings.perfdata[place];
        if settings.file.is_none() {
            return;
        }
        let line = self.expand(&settings.template.clone(), obj, now);
        out.push(Effect::Perfdata {
            host: place == 0,
            line,
        });
    }

    /// Plans the processing commands of the performance data files.
    pub(crate) fn plan_perfdata_processing(&mut self) {
        for place in 0..2 {
            let settings = &self.settings.perfdata[place];
            if settings.processing_interval > 0 && settings.processing_command.is_some() {
                let at = self.start + settings.processing_interval as Ms * 1000;
                self.add_timer(at, Timer::PerfdataProcessing(place as u8));
            }
        }
    }

    /// Runs a processing command, and plans the next run.
    pub(crate) fn process_perfdata(&mut self, place: u8, at: Ms, now: Ms, out: &mut Vec<Effect>) {
        let settings = self.settings.perfdata[place as usize].clone();
        let Some(command) = settings.processing_command else { return };
        // The command's macros are seen from the first host, as no object
        // goes with it.
        if !self.hosts.is_empty() {
            if let Some(line) = self.command_line(&command, Obj::Host(0), now) {
                self.run_job(Obj::Host(0), JobKind::PerfdataProcessing, line, 30_000, now, out);
            }
        }
        self.add_timer(at + settings.processing_interval as Ms * 1000, Timer::PerfdataProcessing(place));
    }
}
