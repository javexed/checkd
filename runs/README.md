# Runs against the reference

The phase 2 gate is "the same state-change log lines as Naemon on the
scripted runs". This directory holds those runs and what compares them.

- `scenarios/`: 51 small configs written for this project, each with a
  recording of what the reference did on it.
- `lib/plugins/`: the scripted check plugins the configs call.
- `real`, `real-entry.sh`: runs checkd itself on the scenarios.
- `otel`: shows that an OpenTelemetry collector takes what checkd sends.
- `timeperiods/`: timeperiods against the reference, see
  [timeperiods/README.md](timeperiods/README.md).

The reference is Naemon Core 1.5.2, as packaged, in the oracle image
(`oracle/README.md`). Every container here runs without a network.

## A scenario

    scenarios/<name>/
        naemon.cfg common.cfg objects.cfg   the config
        resource.cfg fixtures/              where the scenario needs them
        expected/naemon.log                 the reference's log of its run
        expected/trace                      every call of a scripted plugin
        expected/start                      when the run began, ns since 1970
        expected/oracle.version             the reference's version
        compare                             what is not compared, and why

`oracle/play <scenario> <out-dir> <seconds>` makes the recording: it
starts the reference as a daemon on the config, stops it after the
seconds given and copies out what it wrote. The recordings here were made
that way and copied in by hand. Nothing checks that a recording is still
what the reference would do today; to see, record it again and compare.

The hosts and services have invented names and the addresses are from
192.0.2.0/24. No check leaves the container.

### The scripted plugins

A check in a scenario is one of four scripts. Each is called with a key
that names it in the trace.

- `seq <key> <steps>`: the result depends on how often it was called.
- `at <key> <code> [<second>:<code>]...`: the result depends on the
  second of the run.
- `args <key> ...`: writes down what it was called with. This is how a
  scenario records what a macro held at that moment.
- `raw <key> <code|kill> <fixture|-> [wait]`: prints a file as it is,
  waits, and ends with a code or by killing itself.

Each writes one line to the trace as it ends: the millisecond since the
start, its key, and what it was called with or returned. A few scenarios
call other programs (`/bin/echo`, a shell line, a program that is not
there). Those leave nothing in the trace and are compared by the log
alone.

## Two ways of comparing

### On a simulated clock

    cargo test -p checkd-core --test scenarios
    SCENARIO=<name> cargo test -p checkd-core --test scenarios scenarios -- --nocapture

The test drives the engine itself, with no processes and no waiting. It
knows what each scripted plugin would do and gives the engine that
result 2 ms after the check was started. The second form shows both
sides in full.

Five scenarios need real programs or a real shell and are not played
here (`skip` in their `compare` file). The other 46 are.

### In real time

    runs/real [<scenario>...]

This runs the `checkd` binary on each scenario inside the oracle image,
where the reference made its recordings: the same shell, the same paths,
the same plugins. Nothing of the reference is run. Each run takes as long
as the reference's did, eight at a time, about eleven minutes for all 51.
What a run wrote stays in `runs/real-work/<scenario>`.

One scenario is not compared here (`skip-real`): two results that depend
on each other fall 2 ms apart in the recording, which a real run cannot
be held to. For the same reason three more leave out, in the real run
only, the calls or log lines that hang on such an order
(`ignore-real`, `ignore-log-real`): `check-period-opens-and-closes`,
`host-flap-record-one-service` and `host-flap-record-two-services`.

## What is compared

**Log lines.** Per host or service, the lines of these kinds, in order:
`INITIAL HOST/SERVICE STATE`, `HOST/SERVICE ALERT`, `HOST/SERVICE
FLAPPING ALERT`, `HOST/SERVICE EVENT HANDLER`, `GLOBAL HOST/SERVICE EVENT
HANDLER`, `HOST/SERVICE INFO`, and the warning about a return code out of
bounds. The whole text of the line must be the same, plugin output
included. Lines of other kinds are not compared: the start and stop
lines, and warnings about the config.

**Plugin calls.** Per key, the calls in order, each with what the plugin
was called with or returned.

Both lists must have the same entries in the same order. Two entries
that go together must be no more than 1.1 s apart, counted from the
start of each run. A log line carries a whole second only, which is why
the allowance is that wide. What happens in the last 2.2 s of a run may
be on one side only. The order of lines between two objects is not
compared.

Not compared at all: the status file, the saved state, the object cache,
and anything the reference does that leaves no line in its log.

## What checkd is told beforehand

The reference spreads the first checks of a run over its first seconds,
in part at random. checkd spreads them by a rule of its own: each object
in turn, over its check interval. So that the two runs can be laid side
by side, checkd is told when the reference first checked each object:

- In the play on the simulated clock each object's first check is set to
  the millisecond of the reference's first call. After that a check of
  checkd's is moved to the moment of the reference's own when that is no
  more than 40 ms later or 3 ms earlier. The reference is a few
  milliseconds late with every check and counts the next one from there;
  without this the two sides drift apart over a run. Two results that
  come in at the same moment are read in the order the reference's trace
  has them.
- In real time `checkd run` gets the same first-check times through
  `--first-checks`, and `--start-at` sets its clock to the moment the
  recording began, since some scenarios depend on the time of day. Both
  options are for these tests only. Nothing is moved after the first
  check.

This means the comparison does not test checkd's own spread of the first
checks, and the play on the simulated clock does not test drift.

## The `compare` file

One directive to a line, `#` for comments. The reason at the end of a
line is for the reader.

| Directive | Effect |
|---|---|
| `skip <reason>` | not played on the simulated clock |
| `skip-real <reason>` | the real run is not compared |
| `ignore <key>` | calls of this plugin are not compared |
| `ignore-real <key>` | the same, in the real run only |
| `ignore-log <object>` | log lines of this host or `host;service` are not compared |
| `ignore-log-real <object>` | the same, in the real run only |
| `loose <key>` | calls are compared by time, not by content |
| `drop <key>@<ms>` | a call the reference made and checkd is not to make |
| `first <key>@<ms> <reason>` | the object's first check, where the reference made none in the run |
| `first-of <object>@<ms> <reason>` | the same for a check that leaves no trace |
| `real <object> <reason>` | the object's check is a real program; the play does not judge it |
| `mask <key>[@<ms>] <fields> <reason>` | these `name=value` fields of a call are not compared |
| `mask-real ...` | the same, in the real run only |
| `watch <key>` | see below |

`first` times come from the state the reference saved when it was
stopped: its planned next check of an object it had not yet checked.

**`watch`.** Some scenarios have a check that looks at another object,
started within a millisecond or two of that object's own result. Whether
it sees the object before or after the result is not the same from call
to call, in the reference's recording and in a real run of checkd. On
the simulated clock a watched call may show what its neighbour before or
after shows on the other side. In real time the rule is looser: the two
lists must fit one series of states, each call having seen the object
before or after one result. That lets through any single call that
differs between two that agree. The number of calls and their times are
still compared.

## Where checkd does not do what the reference did

Each of these is written down in the scenario's `compare` file. They were
decided while the engine was written and have not been put to anyone who
runs a config that depends on them.

1. **No two checks of one object at once.** Where a service check runs
   longer than its interval the reference starts it again while the
   earlier run is still going. checkd plans the next check when the
   result is in. The reference itself does not overlap host checks.
   Scenarios `checks-longer-than-interval` (three services, and the host
   that is checked when they turn bad, are not compared) and
   `default-settings-slow-checks` (three calls dropped).
2. **The order of group members in macros.** The reference gives the
   services of a service group in the reverse of the order it added them
   in, and the contacts of a contact group in an order of its own with
   the first one twice. checkd gives members by name, each once. The same
   goes for the order of the groups an object is in, and so for which
   group `$HOSTGROUPNAME$` and `$SERVICEGROUPNAME$` name. Scenarios
   `macros-group-members`, `macros-object`, `macros-totals-and-time`.
3. **The time in a state before the first check.** The reference counts
   `$SERVICEDURATION$` from 1970 until the service is first checked.
   checkd counts from its own start. Scenario `macros-object`.
4. **The first check when a check period is closed.** The reference
   puts it at a moment of its own choosing after the period opens. checkd
   checks when the period opens. The scenarios give checkd the
   reference's time. Scenarios `check-period-opens-and-closes`,
   `check-period-opens-spread`.
5. **Timeperiods.** Six forms of line, see
   [timeperiods/README.md](timeperiods/README.md). checkd gives a warning
   at the start for a period that has one of them.

Measured times (latency, execution time) are masked in the real run, and
on the simulated clock they are what the test makes them.

## What the scenarios do not cover

- Anything of phase 3 or later: notifications, downtimes,
  acknowledgements, passive results, external commands, saved state.
  The `$TOTAL...UNHANDLED$` macros depend on those and are not compared.
- `max_concurrent_checks`, freshness checks, obsessing, performance data,
  environment macros, aggressive host checking, `use_timezone`,
  `host_down_disable_service_checks` and passive host check settings.
  checkd warns at the start that it does not act on these yet.
- Predictive dependency checks, service parents, and a time zone other
  than UTC.
- Load. The largest scenario has a few dozen objects.
- A config from a running installation. Every scenario was written to
  show one thing.

## The OpenTelemetry export

    runs/otel [<scenario>]

Builds checkd with the `otel` feature, runs a scenario for a quarter of a
minute in the oracle image and sends to a collector in a second
container. The collector's container has no network and checkd's joins
it, so the two can reach each other and nothing else. The script looks in
what the collector printed for the metrics and spans checkd should have
sent, and fails if one is missing or the collector reported an error.

The collector is the OpenTelemetry project's image
`otel/opentelemetry-collector`, pinned by digest in the script and pulled
from Docker Hub the first time. It is a test tool and no part of checkd.
