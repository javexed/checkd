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
        if self.export && self.item(obj).cfg.process_perf_data {
            let perfdata = self.item(obj).output.perfdata.clone();
            if !perfdata.is_empty() {
                let (host, service) = match obj {
                    Obj::Host(at) => (self.hosts[at].name.clone(), String::new()),
                    Obj::Service(at) => (self.services[at].host_name.clone(), self.services[at].description.clone()),
                };
                out.push(Effect::Sample {
                    host,
                    service,
                    time: now.div_euclid(1000),
                    perfdata,
                });
            }
        }
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

impl Engine {
    /// Plans the first looks at freshness, an interval after the start.
    pub(crate) fn plan_freshness(&mut self) {
        let host = self.settings.host_freshness_check_interval as Ms * 1000;
        let service = self.settings.service_freshness_check_interval as Ms * 1000;
        self.add_timer(self.start + host, Timer::Freshness(0));
        self.add_timer(self.start + service, Timer::Freshness(1));
    }

    /// Looks at the freshness of every host or every service that asks for
    /// it (`check_freshness`): where the last result is older than the
    /// threshold, the object's check is forced at once, also with active
    /// checks off. The threshold is the object's `freshness_threshold`, or,
    /// where that is 0, its check interval (its retry interval in a soft
    /// problem) and `additional_freshness_latency`. An object not checked
    /// since the start counts from the start.
    pub(crate) fn check_freshness(&mut self, hosts: bool, at: Ms, now: Ms, out: &mut Vec<Effect>) {
        let (on, interval) = if hosts {
            (self.settings.check_host_freshness, self.settings.host_freshness_check_interval)
        } else {
            (self.settings.check_service_freshness, self.settings.service_freshness_check_interval)
        };
        self.add_timer(at + interval as Ms * 1000, Timer::Freshness(if hosts { 0 } else { 1 }));
        if !on {
            return;
        }
        let second = now.div_euclid(1000);
        let objects: Vec<Obj> =
            if hosts { (0..self.hosts.len()).map(Obj::Host).collect() } else { (0..self.services.len()).map(Obj::Service).collect() };
        for obj in objects {
            let item = self.item(obj);
            if !item.cfg.check_freshness || item.cfg.check_command.is_none() || item.running || item.freshening {
                continue;
            }
            if let Some(period) = &item.cfg.check_period {
                if !self.periods().is_valid(period, second) {
                    continue;
                }
            }
            let threshold = if item.cfg.freshness_threshold > 0 {
                item.cfg.freshness_threshold as i64
            } else {
                let units = if item.hard || item.state == 0 { item.cfg.check_interval } else { item.cfg.retry_interval };
                (units * self.settings.interval_length) as i64 + item.latency as i64 + self.settings.additional_freshness_latency as i64
            };
            let since = if item.checked { item.last_check } else { self.start };
            let age = second - since.div_euclid(1000);
            if age <= threshold {
                continue;
            }
            let what = match obj {
                Obj::Host(at) => format!("host '{}'", self.hosts[at].name),
                Obj::Service(at) => format!("service '{}' on host '{}'", self.services[at].description, self.services[at].host_name),
            };
            let kind = if hosts { "host" } else { "service" };
            out.push(Effect::Log(format!(
                "Warning: The results of {what} are stale by {} (threshold={}).  I'm forcing an immediate check of the {kind}.",
                span(age - threshold),
                span(threshold)
            )));
            self.item_mut(obj).freshening = true;
            self.check_at(obj, now, true);
        }
    }
}

/// Seconds as the reference writes a span: `0d 0h 1m 5s`.
fn span(seconds: i64) -> String {
    let s = seconds.max(0);
    format!("{}d {}h {}m {}s", s / 86_400, s % 86_400 / 3600, s % 3600 / 60, s % 60)
}

