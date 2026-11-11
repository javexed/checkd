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
- When PostgreSQL cannot be reached, writes wait in memory, not in a
  journal file on disk, and a restart in that time loses them.
- Moving a store from SQLite to PostgreSQL by a command.
- A root certificate of one's own for PostgreSQL's TLS.
