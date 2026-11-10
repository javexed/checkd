//! The `checkd` command. For now it can only check a configuration.

use std::path::PathBuf;
use std::process::ExitCode;

use checkd_config::maincfg::PathMap;
use checkd_config::{Options, dump, load};

const USAGE: &str = "\
usage: checkd verify [options] <main-config-file>

Reads a Nagios or Naemon configuration and reports what is wrong with it.
Exits 0 when the configuration can be used, 1 when it has errors, and 2
when checkd could not be run as asked.

options:
  --dump <file>         write the resolved objects to <file>, in the layout
                        of Naemon's precached object file
  --path-map <from>=<to>
                        read files that the configuration names under the
                        absolute path <from> from <to> instead; can be
                        given more than once
  --quiet               do not print warnings
";

struct Args {
    main: PathBuf,
    dump: Option<PathBuf>,
    path_map: PathMap,
    quiet: bool,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    match args.next().as_deref() {
        Some("verify") => {}
        Some("--version") => return Err(format!("checkd {}", env!("CARGO_PKG_VERSION"))),
        Some(other) => return Err(format!("'{other}' is not a checkd command\n\n{USAGE}")),
        None => return Err(USAGE.to_string()),
    }
    let (mut main, mut dump, mut path_map, mut quiet) = (None, None, PathMap::default(), false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dump" => {
                dump = Some(PathBuf::from(
                    args.next().ok_or("--dump needs a file name")?,
                ))
            }
            "--path-map" => {
                let value = args.next().ok_or("--path-map needs <from>=<to>")?;
                let (from, to) = value
                    .split_once('=')
                    .ok_or("--path-map needs <from>=<to>")?;
                path_map.0.push((PathBuf::from(from), PathBuf::from(to)));
            }
            "--quiet" => quiet = true,
            "--help" | "-h" => return Err(USAGE.to_string()),
            _ if arg.starts_with("--") => {
                return Err(format!("'{arg}' is not an option of verify\n\n{USAGE}"));
            }
            _ if main.is_some() => {
                return Err("verify takes one main configuration file".to_string());
            }
            _ => main = Some(PathBuf::from(arg)),
        }
    }
    let main = main.ok_or_else(|| format!("verify needs a main configuration file\n\n{USAGE}"))?;
    Ok(Args {
        main,
        dump,
        path_map,
        quiet,
    })
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{}", message.trim_end());
            return ExitCode::from(2);
        }
    };
    let loaded = load(
        &args.main,
        &Options {
            path_map: args.path_map,
        },
    );
    if !args.quiet {
        for warning in &loaded.diags.warnings {
            println!("warning: {warning}");
        }
    }
    for error in &loaded.diags.errors {
        println!("error: {error}");
    }
    let (errors, warnings) = (loaded.diags.errors.len(), loaded.diags.warnings.len());
    let Some(objects) = loaded.objects else {
        println!("\n{errors} errors, {warnings} warnings. This configuration cannot be used.");
        return ExitCode::from(1);
    };
    println!(
        "\n{} hosts, {} services, {} contacts, {} commands, {} timeperiods; \
         {} host groups, {} service groups, {} contact groups; \
         {} host and {} service dependencies; {} host and {} service escalations.",
        objects.hosts.len(),
        objects.services.len(),
        objects.contacts.len(),
        objects.commands.len(),
        objects.timeperiods.len(),
        objects.hostgroups.len(),
        objects.servicegroups.len(),
        objects.contactgroups.len(),
        objects.host_dependencies.len(),
        objects.service_dependencies.len(),
        objects.host_escalations.len(),
        objects.service_escalations.len(),
    );
    println!("0 errors, {warnings} warnings. This configuration can be used.");
    if let Some(path) = args.dump
        && let Err(err) = std::fs::write(&path, dump::dump(&objects))
    {
        eprintln!("cannot write {}: {err}", path.display());
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}
