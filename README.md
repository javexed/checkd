# checkd

A monitoring engine in Rust that aims to replace the Naemon / Nagios Core
daemon while staying compatible with the Nagios ecosystem: plugins, object
config, external commands, Livestatus and Thruk.

**Status:** the config loader reads an object config. On 279 of the 293
cases in the corpus it agrees with Naemon Core 1.5.2: the same accept or
refuse, and the same objects. The other fourteen it refuses on purpose
where Naemon accepts: five of them define a service twice, and three have
a fault in an included file. On eleven of twelve config trees that other
people published it gives the same result as Naemon, after the loader was
changed to fit them. The twelfth defines a service twice, so Naemon loads
it and checkd does not. It also gives the same result on the config that
the Naemon package installs and on the config of an OMD demo site, each
read through its own main config: see [real/README.md](real/README.md).

The phase 1 gate has not been judged. It calls for real config trees as
well as written ones. Twelve small published trees and two configs as
shipped are a start. The OMD demo site defines no hosts, and no tree from
a running installation has been tried. Work went on to phase 2 with the
gate in that state.

`checkd run` runs the checks of a config and logs state changes: soft and
hard states, checks of a host on demand, parents, execution dependencies,
flap detection, event handlers, macros and timeperiods. It sends
notifications, with escalations; takes acknowledgements, downtimes,
comments, passive results and check commands through its command file
and through Livestatus; reads check result files; and writes performance
data files. On 61 scripted runs written for this project it is compared
with recordings of Naemon Core 1.5.2 on the same configs, log line by log
line and plugin call by plugin call, notifications, the macros of
notification commands and the performance data included. On a simulated
clock 56 of them play the same as the recording; the other five need
real programs and are left to the real run. Run for real, with the real
plugins and the real command file, 59 were compared and all agreed in the
last full run, within the allowances `runs/README.md` gives; one is not
compared in real time, and the newest, about what a Livestatus user may
see, was run on its own since and agrees.
[runs/README.md](runs/README.md) says what is compared and how closely,
what checkd is told beforehand to make the runs comparable, and where
checkd does something else than Naemon on purpose.

Its state is kept in SQLite or PostgreSQL and survives a kill: see
[docs/store.md](docs/store.md). It reads the `retention.dat` of an
existing Naemon once, when it starts on an empty store. Two nodes on one
PostgreSQL database fail over: the standby took over 14 seconds after the
leader was killed, and the kill and partition tests showed one leader at
a time: see [docs/failover.md](docs/failover.md).

Beyond parity: an HTTP API on the Livestatus tables, with commands
behind a token, `/metrics` for Prometheus, and performance data sent
straight to Graphite or InfluxDB: see [docs/api.md](docs/api.md).
Remote workers run checks, event handlers and notifications on other
machines, in place of mod_gearman, over TLS with a token per worker: see
[docs/workers.md](docs/workers.md). They are tested with an engine and
three workers in containers, one of them killed half way through a
check; not yet with two failover nodes, or under load.

It answers Livestatus, limiting each query to what its `AuthUser` may
see as Naemon does, at the socket of a config's Livestatus
`broker_module` line. The 108 queries Thruk was seen sending for 54 of
its pages are answered as the reference answers them, value by value,
but for measured values and the program's own counters; Thruk renders
all 54 pages against checkd, and 45 are the same as against Naemon after
times and the program's name are taken out. `checkd run --shadow` runs
beside a production engine and reports where states differ. See
[livestatus/README.md](livestatus/README.md), which lists every
difference and why.

A number of
settings that belong to checks are not acted on yet, and `checkd run`
warns when a config sets one of them:
[crates/checkd-core/README.md](crates/checkd-core/README.md). It has not
been run on a config from a real installation, or under load.

## Try it

    cargo build
    target/debug/checkd verify /path/to/naemon.cfg

`checkd verify` reads the main config and everything it names, prints
warnings and errors, and says whether the config can be used. It exits 0
when it can, 1 when it cannot, and 2 when the command itself was wrong.

    --dump <file>          write the resolved objects in Naemon's precache layout
    --path-map <from>=<to> read files under <from> from <to> instead
    --quiet                print errors only

See [crates/checkd-config/README.md](crates/checkd-config/README.md) for
where the loader differs from Naemon on purpose.

    target/debug/checkd run /path/to/naemon.cfg

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

    target/debug/checkd state sqlite:/path/to/checkd.sqlite

prints what a store holds.

Before running it on a config that was written for Naemon, run `checkd
verify` and read the warnings of `checkd run` at its start: they name the
settings and the timeperiod lines that checkd treats otherwise.

### OpenTelemetry

    cargo build --features otel -p checkd
    target/debug/checkd run --otlp-endpoint http://127.0.0.1:4318 /path/to/naemon.cfg

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

## Design

The design document is kept outside this repo:
(a private page, not published)

Phases, each with a gate that must pass before the next starts:

1. Config loader
2. Checks and state
3. Notifications and operations
4. Livestatus for Thruk
5. Failover
6. Beyond parity

## Layout

- `crates/checkd-config`: the config loader (phase 1).
- `crates/checkd-core`: the engine (phases 2 and 3). No clock, no
  processes.
- `crates/checkd-livestatus`: the Livestatus query language (phase 4).
- `crates/checkd`: the daemon binary: `verify` and `run`.
- `oracle/`: the Naemon test oracle, see [oracle/README.md](oracle/README.md).
- `corpus/`: config cases, each with the oracle's expected output.
- `runs/`: scripted runs, each with a recording of what the oracle did,
  see [runs/README.md](runs/README.md).
- `livestatus/`: recording what Thruk asks, and rendering Thruk's pages
  against both engines, see [livestatus/README.md](livestatus/README.md).
- `docs/`: [the store](docs/store.md), [failover](docs/failover.md),
  [the HTTP API, metrics and perfdata export](docs/api.md),
  [reading the configuration again](docs/reload.md),
  [remote workers](docs/workers.md).
- `real/`: the harness for config trees that other people wrote. The
  trees themselves are downloaded and stay out of git.

## Dependencies

`regex` in the loader; `tokio`, `libc` and `rusqlite` (with SQLite built
in) in the daemon. The OpenTelemetry export is written by hand on top of
those and adds none. The PostgreSQL store, behind the `postgres` feature,
adds `tokio-postgres`, and `rustls`, `tokio-postgres-rustls` and
`webpki-roots` for TLS. Remote workers, behind the `workers` feature,
add `tokio-rustls` and `rustls` for TLS and `rcgen` for `checkd
make-certs`.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

This is a clean-room project. See [CONTRIBUTING.md](CONTRIBUTING.md) before
changing anything.
