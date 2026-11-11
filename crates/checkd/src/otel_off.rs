//! What stands in for the OpenTelemetry export in a build without the
//! `otel` feature: the same calls, which do nothing.

use std::time::Duration;

use checkd_core::engine::{Job, Outcome};

pub struct Telemetry;

/// Whether this build can export at all.
pub const BUILT_IN: bool = false;

impl Telemetry {
    pub fn start(endpoint: Option<&str>) -> Result<Telemetry, String> {
        match endpoint {
            // The variable in the environment may be meant for other
            // programs. The option is meant for this one.
            Some(_) => Err("--otlp-endpoint: this checkd was built without the OpenTelemetry \
                            export (the otel feature)"
                .to_string()),
            None => Ok(Telemetry),
        }
    }

    pub fn describe(&self) -> Option<String> {
        None
    }

    pub fn job_started(&mut self, _job: &Job, _name: &str, _latency: f64) {}

    pub fn job_done(&mut self, _id: u64, _outcome: &Outcome, _read: u128) {}

    pub fn next_flush(&self) -> Option<Duration> {
        None
    }

    pub fn flush(&mut self) {}

    pub async fn shutdown(&mut self) {}
}

pub fn nanos() -> u128 {
    0
}
