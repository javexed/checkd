# checkd-core

The engine: what is checked when, and what a check result does to the
state of a host or service.

Nothing in this crate touches a clock, a process or a file. A driver
tells the engine the time and the outcome of the commands it asked for,
and the engine answers with log lines and with more commands to run.
`checkd run` drives it on the real clock, the tests on a simulated one.

It was written from Naemon's public documentation and from what the
reference Naemon did on configs written for the purpose. No Naemon or
Nagios source was read. See
[`../../CONTRIBUTING.md`](../../CONTRIBUTING.md).

## Modules

Each module's header comment gives the rules it follows.

- `engine`: scheduling, soft and hard states, checks of a host on
  demand, reachability through parents, execution dependencies, flap
  alerts, event handlers, the log lines.
- `flap`: the percent state change over the last results.
- `output`: a plugin's output split into short output, long output and
  performance data.
- `cmdline`: whether a command line is started directly or through
  `/bin/sh`, and how it is split.
- `macros`: the `$NAME$` places in a command line.
- `timeperiod`: when a timeperiod is valid.
- `settings`: the main-config settings the engine goes by, and their
  defaults.
- `notify`: notifications, escalations, acknowledgements, downtimes and
  comments.
- `commands`: external commands, as they come through the command file.
- `snapshot`: the engine's state as plain values, to keep over a
  restart, and putting it back.
- `retention`: reading the `retention.dat` of the reference version into
  a snapshot.

## What it is tested against

- `tests/scenarios.rs` plays the 58 scenarios of `runs/scenarios` and
  compares log lines and plugin calls with the reference's recordings.
  [`../../runs/README.md`](../../runs/README.md) says what is compared,
  how closely, and where checkd does something else on purpose.
- `tests/timeperiods.rs` asks the timeperiod evaluator 27,496 questions
  the reference was asked. See
  [`../../runs/timeperiods/README.md`](../../runs/timeperiods/README.md).
- The unit tests in `flap`, `output`, `cmdline`, `macros` and `settings`
  hold values taken from the reference's runs.

## Not in it yet

Passive results and the external commands Thruk sends beyond those for
notifications, acknowledgements, downtimes and comments are phase 4.
Custom notifications, triggered downtimes and global notification
handlers are written but not compared with the reference. Notification
dependencies are not done. Of what belongs to checks and state, these
are not done: a limit on checks running at once, freshness
checks, obsessing, performance data handling, environment macros,
aggressive host checking, predictive dependency checks, service parents
and `use_timezone`.

The first checks after a start are spread by a rule of checkd's own, each
object in turn over its check interval. The reference's spread is partly
random and was not copied, and no test looks at checkd's.
