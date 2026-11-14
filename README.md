<img src="docs/logo.svg" alt="checkd logo: a chickadee on a branch" width="128" align="right">

# checkd

A monitoring engine in Rust that takes the place of the Naemon or Nagios
Core daemon and keeps what is around it: the check plugins, the object
config, external commands, Livestatus, and the web interfaces that talk
Livestatus, Thruk among them.

You point it at the `naemon.cfg` you have. It runs the checks, keeps the
states, sends the notifications and answers Livestatus as Naemon does.
Then it does some things Naemon does not: its state survives a kill, two
nodes fail over, checks can run without a plugin, and a job elsewhere can
push its result in.

## Status: young, and honest about it

- **One installation.** checkd runs one small site, its author's. It has
  not run a large site, and no config from someone else's running
  installation has been tried.
- **Tested against Naemon, not against the world.** What it does is
  compared, case by case, with recordings of Naemon Core 1.5.2 (below).
  Where no case was written, nothing is known.
- **Much of the code was written by an AI model** (Claude), directed and
  reviewed by one person: see [CONTRIBUTING.md](CONTRIBUTING.md), also for
  what that means for the clean-room rule.
- **Version 0.0.0.** Options, file layouts and the extensions below may
  change. Nothing is packaged; you build it.

If you run Naemon in production, try `checkd verify` on your config and
`checkd run --shadow` beside your engine before anything else: both only
read.

## How close it is to Naemon

| | What was compared | Result |
|---|---|---|
| Config | 293 written cases, against Naemon's accept or refuse and the objects it builds | 279 the same; 14 refused on purpose where Naemon accepts (a service defined twice, a fault in an included file): [crates/checkd-config/README.md](crates/checkd-config/README.md) |
| Config | 12 config trees other people published, the config the Naemon package installs, an OMD demo site | the same on all but one tree, which defines a service twice: [real/README.md](real/README.md) |
| Engine | 64 scripted runs: states, retries, flapping, dependencies, notifications, escalations, downtimes, macros, performance data. Log line by log line and plugin call by plugin call | on a simulated clock in the unit tests; and for real, with real processes, in CI, where the 63 that can be held to the clock agree: [runs/README.md](runs/README.md) |
| Livestatus | the 108 queries Thruk was seen sending for 54 of its pages | answered value by value as Naemon answers; Thruk renders all 54 pages, 45 of them the same as against Naemon: [livestatus/README.md](livestatus/README.md) |
| Load | a generated config, a no-op plugin | keeps up to about 15,000 checks a second where Naemon keeps up to about 7,500, in about the memory Naemon takes until it nears that limit: [docs/load.md](docs/load.md) |

Known gaps: a number of settings are read and not acted on, and `checkd
run` warns when a config sets one
([crates/checkd-core/README.md](crates/checkd-core/README.md)); Naemon
checks the master of a dependency again where checkd does not
([docs/digest.md](docs/digest.md)); NEB modules are not loaded, so what a
site does through one (mod_gearman, say) needs checkd's own way.

## What it does beyond Naemon

All of it is off until asked for, and a config that uses none of it is a
config Naemon takes too.

- **State that survives.** In SQLite or PostgreSQL, written as things
  happen; it reads an existing `retention.dat` once
  ([docs/store.md](docs/store.md)). The configuration is read again on
  SIGHUP without losing state ([docs/reload.md](docs/reload.md)).
- **Failover.** Two nodes on one PostgreSQL database; the standby took
  over 14 seconds after the leader was killed
  ([docs/failover.md](docs/failover.md)).
- **Remote workers**, in place of mod_gearman, over TLS
  ([docs/workers.md](docs/workers.md)). Not yet tried under load.
- **Checks without a plugin**: ping, a TCP port, a web page, a
  certificate; and a notification to ntfy, Slack or a webhook without a
  script ([docs/builtin.md](docs/builtin.md)).
- **Results pushed in** over HTTP by a job elsewhere, with an alarm when
  none comes ([docs/push.md](docs/push.md)).
- **A service made of others**, OK while enough of its members are
  ([docs/aggregate.md](docs/aggregate.md)), and one that says when a
  disk will be full ([docs/trend.md](docs/trend.md)).
- **Fewer notifications for one cause**: several to one contact as one
  ([docs/digest.md](docs/digest.md)).
- **Downtimes** for everything that fits a pattern, and ones that come
  round with a timeperiod ([docs/downtimes.md](docs/downtimes.md)).
- **An HTTP API** on the Livestatus tables, `/metrics` for Prometheus,
  performance data straight to Graphite or InfluxDB
  ([docs/api.md](docs/api.md)); OpenTelemetry in a build that has it
  (below).
- **Livestatus that waits**: a request with `WaitTrigger` and
  `WaitTimeout` is answered when something happens, so an interface
  shows a change at once. `check`, `log`, `command` and `all` fire on
  those; `state`, `downtime` and `comment` on a log line or a command;
  a request with a `WaitCondition` is answered at once.

A web interface written beside it, nuthatch, is a project of its own.

## What it runs, and as whom

checkd starts the check commands, event handlers and notification
commands of the config it is given, on the machine it runs on, as the
user it runs as: whoever can change that config, or send it external
commands, can make it run programs. It keeps host names, addresses,
plugin output, comments and the log in its store. See
[SECURITY.md](SECURITY.md) for what listens where, and how to report a
problem.

## Build

A recent stable Rust (it is developed on 1.99; the 2024 edition), on Linux.

    cargo build --release -p checkd

The binary is `target/release/checkd`. Features: `postgres` (the
PostgreSQL store and failover), `workers` (remote workers), `otel`
(OpenTelemetry); `tls-checks`, on by default, is TLS for the checks
without a plugin. [docs/install.md](docs/install.md) has unit files for
systemd, as a system service and as a user's.

## Try it

    target/release/checkd verify /path/to/naemon.cfg

`checkd verify` reads the main config and everything it names, prints
warnings and errors, and says whether the config can be used. It exits 0
when it can, 1 when it cannot, and 2 when the command itself was wrong.

    --dump <file>          write the resolved objects in Naemon's precache layout
    --path-map <from>=<to> read files under <from> from <to> instead
    --quiet                print errors only

See [crates/checkd-config/README.md](crates/checkd-config/README.md) for
where the loader differs from Naemon on purpose.

    target/release/checkd run /path/to/naemon.cfg

`checkd run` loads the config and starts its check commands, event
handlers and notification commands on this machine, as the user it is
run as, until it is stopped with Ctrl-C or SIGTERM. It writes the log to
the config's `log_file`, or to standard output when there is none. It
reads external commands from the config's `command_file`, a named pipe
it makes when it is not there.

    --log-file <file>      write the log there instead; - is standard output
    --for <seconds>        stop after this long
    --path-map <from>=<to> as for verify
    --store <where>        sqlite:<file>, postgres:<connection> or none
    --import-retention <file>
                           a retention.dat to start from, see docs/store.md
    --livestatus <socket>  answer Livestatus there; without it, at the
                           socket of a Livestatus broker_module line
    --shadow <socket>      run beside the engine whose Livestatus socket
                           this is, send nothing, and report differences
    --http <addr>          the HTTP API and /metrics, see docs/api.md
    --perfdata-graphite <host:port>, --perfdata-influx <url>
                           send performance data to a metrics store
    --workers <file>       hand jobs to remote workers, see docs/workers.md
    --otlp-endpoint <url>  see below

SIGHUP, or the external command `RESTART_PROGRAM`, makes it read the
configuration again without losing state: see [docs/reload.md](docs/reload.md).

    target/release/checkd state sqlite:/path/to/checkd.sqlite

prints what a store holds.

Before running it on a config that was written for Naemon, run `checkd
verify` and read the warnings of `checkd run` at its start: they name the
settings and the timeperiod lines that checkd treats otherwise.

### OpenTelemetry

    cargo build --features otel -p checkd
    target/release/checkd run --otlp-endpoint http://127.0.0.1:4318 /path/to/naemon.cfg

A build with the `otel` feature can send metrics and traces about checkd
itself to an OpenTelemetry collector, as OTLP over HTTP. Nothing is sent
unless an endpoint is given, with the option or in
`OTEL_EXPORTER_OTLP_ENDPOINT`. A plain build does not have the code.

- Metrics: checks and event handlers run, by kind and outcome; check
  latency; how long commands ran; how many are running.
- Traces: one span for a sampled check or event handler, one in a
  hundred unless `OTEL_TRACES_SAMPLER_ARG` says otherwise. **A span
  carries the name of the host and of the service.** Whoever can read
  the collector can read those.
- Only `http://` endpoints. There is no TLS, so the collector should be
  on the same machine or a network that is trusted. Headers from
  `OTEL_EXPORTER_OTLP_HEADERS` are sent, in the clear like the rest.
- Not there yet: logs, and the metrics the design lists for parts that do
  not exist yet.

## Layout

- `crates/checkd-config`: the config loader.
- `crates/checkd-core`: the engine. No clock, no processes.
- `crates/checkd-livestatus`: the Livestatus query language.
- `crates/checkd`: the daemon: `verify`, `run`, `state`, `check`.
- `oracle/`: the Naemon test oracle, see [oracle/README.md](oracle/README.md).
- `corpus/`: config cases, each with the oracle's expected output.
- `runs/`: scripted runs, each with a recording of what the oracle did,
  see [runs/README.md](runs/README.md).
- `livestatus/`: recording what Thruk asks, and rendering Thruk's pages
  against both engines, see [livestatus/README.md](livestatus/README.md).
- `docs/`: one page for each of the things above, and
  [installing it](docs/install.md).
- `real/`: the harness for config trees that other people wrote. The
  trees themselves are downloaded and stay out of git.

## Dependencies

`regex` in the loader; `tokio`, `libc` and `rusqlite` (with SQLite built
in) in the daemon. The OpenTelemetry export is written by hand on top of
those and adds none. The PostgreSQL store, behind the `postgres` feature,
adds `tokio-postgres`, and `rustls`, `tokio-postgres-rustls` and
`webpki-roots` for TLS. Remote workers, behind the `workers` feature,
add `tokio-rustls` and `rustls` for TLS and `rcgen` for `checkd
make-certs`. The checks without a plugin use `tokio-rustls`, `rustls` and
`webpki-roots` for TLS, behind `tls-checks`.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

This is a clean-room project: nothing in it is taken from the source of
Naemon or Nagios Core, which are GPL. Naemon is used as a test oracle, as
a separate program, and what it printed on configs written for this
project is kept beside the tests. See [CONTRIBUTING.md](CONTRIBUTING.md)
for the rule, its one stated exception, and what is not settled about
it, before changing anything.

Nagios is a trademark of Nagios Enterprises; Naemon and Thruk are their
projects' names. checkd is not affiliated with any of them.
