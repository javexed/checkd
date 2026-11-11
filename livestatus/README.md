# Livestatus for Thruk

checkd answers Livestatus queries itself, at a Unix socket: the one a
config's Livestatus `broker_module` line names, or the one given with
`checkd run --livestatus <socket>`. The phase 4 gate is "Thruk's pages work
unchanged, and a shadow run beside production shows no differences".

## What Thruk asks

    livestatus/record-thruk
    python3 -I livestatus/templates

`record-thruk` starts an OMD Labs site (Naemon, its Livestatus module,
Thruk) in a container without a network, with the objects of
`site/objects.cfg`, sends the commands of `site/commands`, and renders
each page of `pages` with Thruk's own `thruk url`. The Livestatus module,
in its debug mode, logs every query. `templates` turns those queries into
templates, with times written relative to the moment of the query, in
`runs/scenarios/livestatus-thruk/queries`.

The 54 pages made 249 queries, 108 different ones, over 12 tables:
`status`, `hosts`, `services`, `hostgroups`, `servicegroups`,
`contactgroups`, `contacts`, `commands`, `timeperiods`, `comments`,
`downtimes` and `log`. These are checkd's first set: the tables, the
columns those queries use and some that go with them (408 in all, listed
by `GET columns`), and the headers they use: `Columns`, `Filter`, `And`,
`Or`, `Negate`, `Stats` with `sum`, `min`, `max`, `avg`, `std`, `StatsAnd`,
`StatsOr`, `StatsNegate`, `Sort`, `Limit`, `Offset`, `OutputFormat` (`json`,
`wrapped_json`, `csv`), `ResponseHeader: fixed16`, `KeepAlive`,
`ColumnHeaders`, `Separators`, and `COMMAND`. `AuthUser`, `Localtime` and
the `Wait` headers are read and not acted on.

The recordings and the templates stay out of git: they are what Thruk
sent, as the published config trees are what other people wrote.

## Answers, value by value

`runs/scenarios/livestatus-thruk` is a scenario like the others, with
`ask`: at seconds 20 and 40 every template is filled in and put to the
reference, and in `runs/real` to checkd, which runs time-locked with the
reference's recording. Checks are stopped from second 17 to 23 and from
37 on, by external commands, so that no result falls between the queries.
`crates/checkd-livestatus/tests/answers.rs` compares the 216 answers, and
all 216 agree. It does not compare:

- values that are measured: latency and execution time;
- `next_check` and `last_update`, which the reference moves at every check
  it passes over while checks are stopped;
- `check_options` and `is_executing`;
- the program's identity and counters in `status`: version, process id,
  requests, connections, checks run, log messages, and their rates;
- in the `log` table, each program's own messages, and the order and the
  exact second of lines written within a second of each other.

And it lets checkd differ, on purpose:

- **The hard counts of host groups.** For `hostgroups`, the reference
  gives 0 for `num_services_hard_ok`, `_warn`, `_crit`, `_unknown` and
  `worst_service_hard_state` whatever the services' states; the same
  columns of `servicegroups` count as they should. checkd counts for
  host groups too.

What the reference does that checkd copies, learned from its answers:

- A column a table does not have is answered with an empty string, and
  status 200. An unknown table is `404` with `Invalid GET request, no
  such table '<name>'`. An error's answer has the `fixed16` header only
  when `ResponseHeader` came before the faulty line.
- Hosts and services come in the order of their names; the services of
  one host, asked for by `host_name =`, and a host's `services` columns,
  in the reverse order. Comments and downtimes by number, log lines
  newest first.
- `last_time_ok` and `last_time_up` are the program's start before the
  object was ever found up or OK. A state left is stamped with the time
  of the result that left it. A host's `last_state_change` moves with its
  hard state only.
- A fixed downtime's `duration` is its end less its start. A downtime's
  own comment has `source` 0, every other comment 1.
- The `log` table knows certain kinds of line (alerts, notifications and
  the lines about them, downtime and flapping alerts, external commands,
  initial states, passive checks, event handlers). A line of another
  kind has all its text as its type. An external command's line names
  the host and service the command names.
- Floats are written with ten digits after the point and a two-digit
  exponent; `last_update` as seconds with six decimals.

## Thruk's pages

    livestatus/thruk-pages

Renders every page of `pages` with Thruk against Naemon, then against
checkd, in one OMD container, and compares them. Checks are off in the
site's main config; both engines get the same forced check of every host
and service at second 2 and the same commands at second 10, so they have
made the same checks when the pages are made. Before the comparison,
both sides lose times, durations, the program's name, version and process
id, Thruk's per-request tokens, and the order of keys in the JavaScript
objects and hidden form fields Thruk writes, which is not the same from
one rendering to the next. The pages stay out of git: they are Thruk's.

Every page renders against checkd. Of the 54, 45 are the same. The other
nine, and why:

| Page | Why it differs |
|---|---|
| `extinfo.cgi?type=0` | the process id |
| `extinfo.cgi?type=4` | the program's counters of checks and their timing |
| `showlog.cgi` | each program's own messages in its log |
| `summary.cgi?...standardreport=1` | the order of alerts written in the same second |
| `config.cgi?type=servicegroups`, `/thruk/r/servicegroups` | the order of a service group's members: the reference keeps them in the reverse order they were defined in; checkd's config loader keeps them by name |
| `/thruk/r/hostgroups` | the hard counts of host groups, on purpose (above) |
| `/thruk/r/hosts`, `/thruk/r/services` | `last_time_*` and `last_hard_state` after a forced check while all checks are off, which the reference does not stamp; with checks on, the answer test above agrees on both |

Thruk decides which columns to ask for by the backend's
`livestatus_version`: one that ends in `-naemon` gets Naemon's columns and
its `wrapped_json` output. checkd's is `1.5.1-checkd-<version>-naemon`,
and its `program_version` `1.5.2 (checkd <version>)`. This was found by
reading how Thruk (GPL, like Naemon, and not part of checkd) picks its
columns; nothing of Thruk is copied.

## Passive results and performance data

Phase 4 also brings what feeds Livestatus from outside, and what comes
out of every result. `runs/scenarios/passive-results-and-perfdata`
compares them with the reference, on the simulated clock and in real
time:

- `PROCESS_SERVICE_CHECK_RESULT` and `PROCESS_HOST_CHECK_RESULT`, through
  the command file or Livestatus `COMMAND`;
- files in `check_result_path`, as NRDP and the like drop them, each with
  its `.ok` file, read every `check_result_reaper_frequency` seconds;
- the host and service performance data files, by their templates, after
  every result, and their processing commands on their intervals.

What the reference does with them is in the header comment of
`crates/checkd-core/src/passive.rs`. The perfdata lines are compared as a
set, without times and measured values.

## Shadow mode

    runs/shadow/run

`checkd run --shadow <socket>` runs beside a production engine whose
Livestatus socket that is, on the same config. It sends no notifications,
runs no event handlers, keeps no state, leaves the config's command file,
socket and log alone, and every ten seconds compares the state and state
type of every host and service with production's. A difference that
lasts longer than 30 seconds and twice the object's own interval is
written to its log, and so is its end; each round is summed up in a line.

The test runs the reference with its Livestatus module and checkd in
shadow mode beside it for 150 seconds, on `runs/shadow/case`: all eight
hosts and services compared in 14 rounds, no difference. Then again with
checkd given a config in which one service stays OK where production's
turns CRITICAL: that one difference is reported, and no other.

The gate's "beside production" has no production installation here. The
reference stands in for it.

## More tables and commands

`runs/scenarios/livestatus-commands` sends the reference about 60
commands written for this project: switches of hosts and services and of
their groups, the global switches, changes of intervals, attempts and
custom variables, comments deleted, downtimes of groups and of a host's
services. At three moments it asks, with queries also written for this
project, for the switches and `modified_attributes_list`, the comments
and downtimes, the tables `hostsbygroup`, `servicesbygroup` and
`servicesbyhostgroup`, and `statehist`. All 33 answers agree, and so do
the log lines and the errors of bad commands. What the reference does:

- a command marks an attribute modified only when it changes its value;
  the mark stays when a later one sets it back;
- `CHANGE_CUSTOM_*_VAR` changes a variable the object has, and marks the
  object modified even when it has none of that name;
- `DEL_ALL_*_COMMENTS` deletes the comments of downtimes too;
- a notification held back by a command says so: "temporarily disabled
  ... by an external command";
- `statehist` is worked out for the window of the query's `time` filters;
  before the first line about an object it counts as in the state that
  line gives; a span ends at each change of state, soft ones included,
  and at the start and end of a downtime.

These queries are the project's own, so they and the reference's answers
are in git, unlike Thruk's.

## Not done

- The columns that neither Thruk nor these queries ask for.
- `AuthUser` (all is shown to everyone), `Wait` headers, `Localtime`.
- Livestatus over TCP.
- The external commands Thruk sends that are not about acknowledgements,
  downtimes, comments, notifications, checks and passive results.
- Pages beyond the 54, Thruk's panorama, reports and business processes.
