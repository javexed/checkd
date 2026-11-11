//! The checkd engine: what is checked when, and what a check result does
//! to the state of a host or service.
//!
//! Nothing here touches a clock, a process or a file. The engine is told
//! the time and the results of the commands it asked for, and answers
//! with log lines and with more commands to run. A driver does the rest:
//! `checkd run` on the real clock, the test driver on a simulated one.
//!
//! The behaviour is the reference version's, learned from its
//! documentation and from running it on configs written for the purpose.

pub mod cmdline;
pub mod engine;
pub mod flap;
pub mod macros;
pub mod output;
pub mod settings;
pub mod timeperiod;
