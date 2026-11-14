//! Loads Nagios and Naemon configuration: the main configuration file and
//! the object files it names.
//!
//! [`load`] reads the files, applies templates, expands groups and lists,
//! checks the result and returns the objects. [`dump::dump`] writes them
//! in the layout of Naemon's precached object file.
//!
//! This crate was written from the public Naemon documentation and from
//! the output of a stock Naemon on test configurations. See
//! `CONTRIBUTING.md` at the top of the repository.

pub mod build;
pub mod diag;
pub mod dump;
pub mod maincfg;
pub mod model;
pub mod raw;
pub mod resolve;
pub mod schema;
pub mod timeperiod;
pub mod values;

use std::path::Path;
use std::sync::Arc;

use diag::Diagnostics;
use maincfg::{MainConfig, PathMap};
use model::Objects;

/// How to load a configuration.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Where to find files that the configuration names by absolute path.
    pub path_map: PathMap,
}

/// The result of loading a configuration.
#[derive(Debug)]
pub struct Loaded {
    /// `None` when the main configuration file could not be read.
    pub main: Option<MainConfig>,
    /// `None` when there are errors.
    pub objects: Option<Objects>,
    pub diags: Diagnostics,
}

/// Loads a configuration for a program that keeps it for long.
///
/// Loading frees much more than it keeps, and what it keeps lies
/// scattered among what it freed: the C allocator can give almost none of
/// that back, and a large configuration held three times its size. So
/// the loading is done on a thread of its own, whose memory is apart
/// from the caller's; what was loaded is copied, close together, into
/// the caller's; and the thread's memory, then all free, goes back.
pub fn load_compact(main: &Path, options: &Options) -> Loaded {
    let loaded = std::thread::scope(|scope| scope.spawn(|| load(main, options)).join());
    let loaded = match loaded {
        Ok(loaded) => loaded,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    Loaded {
        main: loaded.main.clone(),
        objects: loaded.objects.as_ref().map(Objects::copy),
        diags: loaded.diags.clone(),
    }
}

/// Loads the configuration whose main file is at `main`.
pub fn load(main: &Path, options: &Options) -> Loaded {
    let mut diags = Diagnostics::default();
    let Some(config) = maincfg::read(main, &options.path_map, &mut diags) else {
        return Loaded {
            main: None,
            objects: None,
            diags,
        };
    };

    let mut raw = Vec::new();
    for path in &config.object_files {
        if let Some(text) = maincfg::read_text(path, None, &mut diags) {
            raw::parse_file(&Arc::new(path.clone()), &text, &mut raw, &mut diags);
        }
    }
    let number = |name: &str, default: u32| match config.get(name).map(values::parse_int_strict) {
        Some(Ok(number)) => number,
        _ => default,
    };
    let defaults = build::Settings::default();
    let settings = build::Settings {
        regexp: config.flag("use_regexp_matching"),
        true_regexp: config.flag("use_true_regexp_matching"),
        illegal_name_chars: config
            .get("illegal_object_name_chars")
            .unwrap_or("")
            .to_string(),
        host_check_timeout: number("host_check_timeout", defaults.host_check_timeout),
        service_check_timeout: number("service_check_timeout", defaults.service_check_timeout),
        allow_empty_hostgroups: config
            .get("allow_empty_hostgroup_assignment")
            .is_none_or(maincfg::leading_number_is_positive),
    };
    let objects = build::build(raw, &settings, &mut diags);

    // Naemon accepts both of these without a word.
    if config.object_files.is_empty() {
        diags.warn(
            None,
            "the main configuration names no object files (cfg_file or cfg_dir)",
        );
    } else if objects.hosts.is_empty() && !diags.has_errors() {
        diags.warn(None, "no hosts are defined, so nothing will be checked");
    }
    let objects = (!diags.has_errors()).then_some(objects);
    Loaded {
        main: Some(config),
        objects,
        diags,
    }
}
