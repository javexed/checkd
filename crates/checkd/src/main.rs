//! The `checkd` command: `verify` checks a configuration, `run` runs its
//! checks.

#[cfg(feature = "otel")]
mod otel;
#[cfg(not(feature = "otel"))]
#[path = "otel_off.rs"]
mod otel;
#[cfg(feature = "postgres")]
mod failover;
mod http;
mod livestatus;
mod perfexport;
mod postgres;
mod run;
mod runner;
mod shadow;
mod store;
#[cfg(feature = "workers")]
mod tls;
#[cfg(feature = "workers")]
mod worker;
#[cfg(feature = "workers")]
mod workers;

use std::path::PathBuf;
use std::process::ExitCode;

use checkd_config::maincfg::PathMap;
use checkd_config::{Options, dump, load};

const USAGE: &str = "\
usage: checkd verify [options] <main-config-file>
       checkd run [options] <main-config-file>
       checkd state <store>
       checkd worker [options]
       checkd make-certs <dir> <engine host name>...

verify reads a Nagios or Naemon configuration and reports what is wrong
with it. It exits 0 when the configuration can be used, 1 when it has
errors, and 2 when checkd could not be run as asked.

run runs the checks of a configuration, writes the state changes of its
hosts and services to the log, sends notifications, takes external
commands from the command file, and keeps its state in a store, until it
gets SIGTERM or SIGINT.

state prints what a store holds: sqlite:<file> or postgres:<connection>.

worker runs the jobs of an engine started with --workers, on another
machine; checkd worker --help says how. make-certs writes a certificate
authority and a certificate for the engine's host names into <dir>, for
a site that has none of its own. Both only in a build with the workers
feature.

options of verify:
  --dump <file>         write the resolved objects to <file>, in the layout
                        of Naemon's precached object file
  --path-map <from>=<to>
                        read files that the configuration names under the
                        absolute path <from> from <to> instead; can be
                        given more than once
  --quiet               do not print warnings

options of run:
  --log-file <file>     write the log to <file> instead of the log_file
                        of the configuration; - is standard output, which
                        is also where the log goes when neither is given
  --for <seconds>       stop after this long
  --path-map <from>=<to>
                        as for verify
  --store <where>       keep the state in sqlite:<file>, in
                        postgres:<connection>, or nowhere: none. Without
                        it, in checkd.sqlite beside the configuration's
                        state_retention_file, unless
                        retain_state_information is 0
  --import-retention <file>
                        when the store holds no state yet, take it from
                        this retention.dat of Naemon or Nagios; without
                        it, from the configuration's state_retention_file
                        if there is one
  --journal <file>      with a PostgreSQL store, the SQLite file the state
                        is kept in while the store cannot be written;
                        without it, checkd-journal.sqlite beside the
                        state_retention_file, or in systemd's
                        $STATE_DIRECTORY. Not with --failover
  --livestatus <socket> answer Livestatus queries at this Unix socket;
                        without it, at the socket of a Livestatus
                        broker_module line of the configuration
  --shadow <socket>     run beside a production engine whose Livestatus
                        socket this is: send no notifications, run no
                        event handlers, keep no state, and write to the
                        log where the states of hosts and services differ
                        from production's
  --failover <node>     take part in failover as the node of this name:
                        lead while holding the lease in the PostgreSQL
                        store, stand by otherwise. Ends with status 3
                        when the lease is lost, to be started again
  --lease <seconds>     how long the lease lasts unrenewed (15)
  --peer-listen <addr>  where to answer the other node's heartbeat
                        (0.0.0.0:6558)
  --peer <addr>         where the other node answers its heartbeat
  --livestatus-tcp <addr>
                        answer Livestatus over TCP too. With --failover:
                        for the other node to forward to (0.0.0.0:6557).
                        Without: for any client, on a loopback address
                        unless CHECKD_LIVESTATUS_ALLOW_NETWORK=1, since
                        Livestatus has no login and takes commands
  --advertise <host>    the name the other node reaches this one at
                        (the node's name)
  --isolated keep|hold  when neither the database nor the other node
                        answers: go on notifying (keep), or hold
                        notifications back (hold)
  --http <addr>         answer the HTTP API and /metrics at this address;
                        commands need the token in CHECKD_HTTP_TOKEN
  --perfdata-graphite <host:port>
                        send the performance data of results to Graphite
  --perfdata-influx <url>
                        send them to InfluxDB, as line protocol posted to
                        this http:// URL; CHECKD_INFLUX_TOKEN is sent when
                        set
  --workers <file>      hand checks, event handlers and notifications to
                        remote workers, as the file says (docs/workers.md).
                        Only in a build with the workers feature.
  --otlp-endpoint <url> send metrics and traces about checkd itself to
                        the OpenTelemetry collector at this http:// URL,
                        as OTLP over HTTP; without it the URL in
                        OTEL_EXPORTER_OTLP_ENDPOINT is used, and without
                        either nothing is sent. Only in a build with the
                        otel feature.

options of run for tests that compare its runs with another engine's:
  --first-checks <file> check each object first at the moment <file>
                        gives for it, one
                        '<milliseconds since 1970> <host>[;<service>]'
                        to a line
  --start-at <milliseconds since 1970>
                        take the moment of the start for this time, and
                        count on from there
";

struct Args {
    main: PathBuf,
    dump: Option<PathBuf>,
    path_map: PathMap,
    quiet: bool,
}

enum Asked {
    State(String),
    Verify(Args),
    Run(run::RunArgs),
    #[cfg(feature = "workers")]
    Worker(worker::WorkerArgs),
    #[cfg(feature = "workers")]
    MakeCerts(PathBuf, Vec<String>),
}

fn parse_run(mut args: impl Iterator<Item = String>) -> Result<run::RunArgs, String> {
    let (mut main, mut path_map) = (None, PathMap::default());
    let (mut log_file, mut seconds, mut first_checks, mut start_at) = (None, None, None, None);
    let mut otlp_endpoint = None;
    let (mut store, mut import, mut livestatus, mut shadow) = (None, None, None, None);
    let mut http = None;
    let mut workers = None;
    let mut journal = None;
    let mut livestatus_tcp: Option<String> = None;
    let (mut perfdata_graphite, mut perfdata_influx) = (None, None);
    let mut failover = run::Failover {
        lease: 15.0,
        peer_listen: "0.0.0.0:6558".into(),
        livestatus_tcp: "0.0.0.0:6557".into(),
        ..Default::default()
    };
    while let Some(arg) = args.next() {
        let mut value = |what: &str| args.next().ok_or(format!("{arg} needs {what}"));
        match arg.as_str() {
            "--log-file" => log_file = Some(PathBuf::from(value("a file name")?)),
            "--otlp-endpoint" => otlp_endpoint = Some(value("a URL")?),
            "--store" => store = Some(value("sqlite:<file>, postgres:<connection> or none")?),
            "--import-retention" => import = Some(PathBuf::from(value("a file name")?)),
            "--livestatus" => livestatus = Some(PathBuf::from(value("a socket path")?)),
            "--shadow" => shadow = Some(PathBuf::from(value("a socket path")?)),
            "--http" => http = Some(value("an address")?),
            "--workers" => workers = Some(PathBuf::from(value("a file name")?)),
            "--journal" => journal = Some(PathBuf::from(value("a file name")?)),
            "--perfdata-graphite" => perfdata_graphite = Some(value("host:port")?),
            "--perfdata-influx" => perfdata_influx = Some(value("a URL")?),
            "--failover" => failover.node = value("the name of this node")?,
            "--lease" => {
                let text = value("a number of seconds")?;
                failover.lease = text.parse().map_err(|_| format!("--lease needs a number of seconds, not '{text}'"))?;
            }
            "--peer-listen" => failover.peer_listen = value("an address")?,
            "--peer" => failover.peer = Some(value("an address")?),
            "--livestatus-tcp" => {
                let addr = value("an address")?;
                failover.livestatus_tcp = addr.clone();
                livestatus_tcp = Some(addr);
            }
            "--advertise" => failover.advertise = value("a host name")?,
            "--isolated" => {
                failover.hold = match value("keep or hold")?.as_str() {
                    "keep" => false,
                    "hold" => true,
                    other => return Err(format!("--isolated takes keep or hold, not '{other}'")),
                }
            }
            "--first-checks" => first_checks = Some(PathBuf::from(value("a file name")?)),
            "--for" => {
                let text = value("a number of seconds")?;
                seconds = Some(
                    text.parse()
                        .map_err(|_| format!("--for needs a number of seconds, not '{text}'"))?,
                );
            }
            "--start-at" => {
                let text = value("a time in milliseconds")?;
                start_at = Some(text.parse().map_err(|_| {
                    format!("--start-at needs a time in milliseconds, not '{text}'")
                })?);
            }
            "--path-map" => {
                let value = value("<from>=<to>")?;
                let (from, to) = value
                    .split_once('=')
                    .ok_or("--path-map needs <from>=<to>")?;
                path_map.0.push((PathBuf::from(from), PathBuf::from(to)));
            }
            "--help" | "-h" => return Err(USAGE.to_string()),
            _ if arg.starts_with("--") => {
                return Err(format!("'{arg}' is not an option of run\n\n{USAGE}"));
            }
            _ if main.is_some() => {
                return Err("run takes one main configuration file".to_string());
            }
            _ => main = Some(PathBuf::from(arg)),
        }
    }
    let main = main.ok_or_else(|| format!("run needs a main configuration file\n\n{USAGE}"))?;
    Ok(run::RunArgs {
        main,
        path_map,
        log_file,
        seconds,
        first_checks,
        start_at,
        otlp_endpoint,
        command_file: None,
        store,
        import,
        livestatus,
        shadow,
        http,
        workers,
        journal,
        // Without failover, a TCP address given is one for any client.
        livestatus_tcp: if failover.node.is_empty() { livestatus_tcp } else { None },
        perfdata_graphite,
        perfdata_influx,
        failover: (!failover.node.is_empty()).then(|| {
            if failover.advertise.is_empty() {
                failover.advertise = failover.node.clone();
            }
            failover
        }),
    })
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Asked, String> {
    match args.next().as_deref() {
        Some("verify") => {}
        Some("run") => return parse_run(args).map(Asked::Run),
        Some("state") => {
            return match (args.next(), args.next()) {
                (Some(spec), None) => Ok(Asked::State(spec)),
                _ => Err(format!("state takes one store\n\n{USAGE}")),
            };
        }
        #[cfg(feature = "workers")]
        Some("worker") => return worker::parse(args).map(Asked::Worker),
        #[cfg(feature = "workers")]
        Some("make-certs") => {
            let dir = args.next().ok_or(format!("make-certs needs a directory\n\n{USAGE}"))?;
            let names: Vec<String> = args.collect();
            if names.is_empty() {
                return Err(format!("make-certs needs the engine's host names\n\n{USAGE}"));
            }
            return Ok(Asked::MakeCerts(PathBuf::from(dir), names));
        }
        #[cfg(not(feature = "workers"))]
        Some("worker" | "make-certs") => {
            return Err("this checkd was built without remote workers (the workers feature)".to_string());
        }
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
    Ok(Asked::Verify(Args {
        main,
        dump,
        path_map,
        quiet,
    }))
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(Asked::Verify(args)) => args,
        Ok(Asked::Run(args)) => return run::run(args),
        Ok(Asked::State(spec)) => return run::show_state(&spec),
        #[cfg(feature = "workers")]
        Ok(Asked::Worker(args)) => return worker::run(args),
        #[cfg(feature = "workers")]
        Ok(Asked::MakeCerts(dir, names)) => {
            return match tls::make_certs(&dir, &names) {
                Ok(()) => {
                    println!(
                        "Wrote ca.crt, ca.key, engine.crt and engine.key to {}. Give workers ca.crt; keep ca.key away from the engine.",
                        dir.display()
                    );
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("{err}");
                    ExitCode::from(2)
                }
            };
        }
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
