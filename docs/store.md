# The store

checkd keeps its state in a store so that a restart, or a crash, loses
nothing that matters: the state of every host and service, notification
counts, acknowledgements, downtimes, comments, and the switches that
commands turned on or off. The store also holds the log, with a time
index, for reports.

## Choosing one

    checkd run --store sqlite:/var/lib/checkd/checkd.sqlite naemon.cfg
    checkd run --store 'postgres:host=db.example user=checkd dbname=checkd' naemon.cfg
    checkd run --store none naemon.cfg

Without `--store`, checkd uses an SQLite file named `checkd.sqlite` beside
the configuration's `state_retention_file`. With
`retain_state_information=0`, or no `state_retention_file`, it keeps no
state.

- **SQLite** is one file, written ahead (WAL). It needs nothing else.
- **PostgreSQL** needs a build with the `postgres` feature
  (`cargo build --features postgres`). The connection is given in the
  keyword form or as a URL. The password is not written there, where
  `ps` would show it: it comes from `PGPASSWORD`, or from the file that
  `CHECKD_STORE_PASSWORD_FILE` names. A database on another machine is
  reached with TLS only, checked against the usual public root
  certificates; `sslmode=disable` is taken for a database on the same
  machine only. checkd uses plain tables and views, no extensions and no
  superuser, so a managed service such as Azure's works.

`checkd state <store>` prints what a store holds.

## What commands changed

Settings that external commands change are kept over a restart and over
a reload, with what the configuration said when they were saved:

- of a host or service: notifications, active and passive checks, event
  handler, flap detection, check and retry intervals, maximum attempts,
  custom variables, and which of these commands touched (Livestatus'
  `modified_attributes`);
- of the program: notifications, the execution of host and service
  checks, passive results, event handlers, flap detection.

A saved value is put back only where the configuration still says what
it said then: a setting changed in the configuration meanwhile takes the
configuration's value. Settings are in the table `settings` (one row per
setting that differs from the configuration) and the program's in `meta`
(`program.<name>`).

From a Naemon `retention.dat` only notifications and active checks of
hosts and services are taken yet; the other settings start as
configured.

## When PostgreSQL cannot be reached

checkd goes on monitoring, and keeps its state in a journal on its own
disk: an SQLite file, `checkd-journal.sqlite` beside the configuration's
`state_retention_file`, or the file given with `--journal`.

- While the database does not answer, each batch of events goes to the
  journal, and the database is tried again every five seconds.
- If checkd is restarted in that time, it starts from the journal, even
  while the database still does not answer, and connects when it does.
- Once the database answers, the state and the log lines the journal
  holds are written to it in one transaction, and the journal is
  removed.
- Without a `state_retention_file` and without `--journal` there is no
  journal: what waits stays in memory, up to 100,000 log lines, and a
  restart in that time loses it.
- Not with `--failover`: only the node that holds the lease writes, and a
  journal of a node that lost the lease would be wrong to replay. The
  other node takes over instead.

`runs/store/outage` cuts checkd off from its database, makes changes,
kills checkd and starts it again while still cut off, then lets it
reach the database, and finds everything there.

## Moving from Naemon or Nagios

When the store holds no state yet, checkd reads the configuration's
`state_retention_file` once, if it is there, or the file given with
`--import-retention`. From it come the states, notification numbers,
acknowledgements, comments, downtimes and the switches changed at run
time. A switch whose configured value has changed since it was saved
takes the configuration's value. Comments that are not persistent are
left out, as Naemon does at a restart, except those of an
acknowledgement or a downtime that is still there. After that the store
is used and the file is not read again.

## When it is written

After each batch of events checkd compares its state with what it last
wrote, and writes what changed in one transaction, with the log lines of
the batch:

- a batch with any line for the log (a change of state, a notification,
  a command) is written at once, before the commands it starts are
  started. A notification is on record before it is sent;
- otherwise, every five seconds. A crash loses at most the last five
  seconds of routine check results, and those are checked again.

When the store cannot be written, checkd says so once in the log and goes
on checking and notifying. Log lines wait in memory for the next write,
up to 100,000 of them.

## The views

The views whose names start with `v1_` are the contract: their columns
keep their names and meanings within version 1. The tables behind them
can change between releases. Times are seconds since 1970.

| View | One row per |
|---|---|
| `v1_host_status` | host: state, state type, attempt, output, last check, last state change, flapping, acknowledged, notification number, switches |
| `v1_service_status` | service, with the same columns and `service` |
| `v1_state_history` | alert, initial or current state, flapping and downtime line of the log |
| `v1_notifications` | notification sent to one contact: time, host, service, contact, line |
| `v1_downtimes` | downtime, waiting or in effect |
| `v1_acknowledgements` | acknowledged host or service: author, comment, sticky, persistent |
| `v1_comments` | comment: kind (`user`, `downtime`, `flapping`, `acknowledgement`), author, text |
| `v1_log` | line of the log, with its kind, host, service and contact where it names them |

For reports that should not slow the engine down, give them a role that
can only read the views, and on PostgreSQL run them on a replica.

## What is not done yet

- The log table grows without end. Partitions by month and a setting
  that drops old ones are to come.
- Moving a store from SQLite to PostgreSQL by a command.
- A root certificate of one's own for PostgreSQL's TLS.
