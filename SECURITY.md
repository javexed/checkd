# Security

## Reporting a problem

Use **"Report a vulnerability"** under the repository's Security tab on
GitHub. That reaches the maintainer privately. Please do not open a
public issue for something that could be used against a running site.

Say what you did, what happened and what you expected; a config that
shows it helps most. There is one maintainer and no promised time to
answer. Only the newest commit of `main` is looked after; there are no
releases yet.

## What checkd is trusted with

checkd runs programs. That is its job, and it decides what counts as a
problem here.

- **The config is code.** A check command, an event handler and a
  notification command are command lines checkd starts, as the user it
  runs as. Whoever can write the config files, or the files they
  include, can run programs as that user. Keep them writable by that
  user's administrators only, and do not run checkd as root.
- **External commands change what it does.** They can turn checks and
  notifications off, set results, schedule downtimes, and change a
  custom variable; they cannot name a new program to run. A custom
  variable that a command line uses as a macro (`$_HOSTX$`) is then
  theirs to set: treat such a variable as an argument anyone with
  command rights can choose. They come in by the command file (a named
  pipe, made for its owner and group only), by Livestatus, and by the
  HTTP API.
- **Plugin output is data.** It goes into the log, the store, Livestatus
  and, through macros, into the command lines of notification commands
  and event handlers, as Naemon does it: characters named in
  `illegal_macro_output_chars` are taken out first. A notification
  command that hands a macro to a shell is as safe as that setting and
  its own quoting. The checks checkd makes itself, and `checkd:notify`,
  never go through a shell.

## What listens, and who may speak to it

| | Off unless | Who may |
|---|---|---|
| Command file | `check_external_commands=1` and `command_file` in the config | whoever may write the pipe |
| Livestatus socket | a Livestatus `broker_module` line, or `--livestatus` | whoever may open the socket. A query's `AuthUser` limits what is shown, but the caller chooses it: the socket itself is the gate |
| Livestatus over TCP | `--livestatus-tcp`, or failover | **anyone who reaches the port**; no password, no TLS. For a trusted network or the loopback only |
| HTTP API | `--http` | with `CHECKD_HTTP_TOKEN` set, whoever has the token; without it, anyone may read and nobody may send commands. Plain HTTP: put a proxy that does TLS in front, off the loopback |
| Pushed results | `--http`, and a service with `_PUSH_TOKEN` | whoever has that service's token, for that one service's state |
| Remote workers | `--workers` | TLS, a certificate authority of the site's, and a token for each worker |
| Failover peer | `--failover` | the other node, over a trusted network: see [docs/failover.md](docs/failover.md) |

## What it stores and sends

- The store (SQLite or PostgreSQL) holds host names, addresses, states,
  plugin output, comments, downtimes and the log. With PostgreSQL on
  another machine, all of that is there too. A store's password is read
  from a file, not from the command line.
- Tokens of pushed results are custom variables in the config: whoever
  can read the config, or custom variables over Livestatus, can read
  them.
- Nothing is sent anywhere unless configured: performance data to
  Graphite or InfluxDB, metrics and traces to an OpenTelemetry collector
  (spans carry host and service names), notifications by `checkd:notify`
  to the address its command line names.
- `checkd:http -S`, `checkd:cert` and `checkd:notify` trust the
  certificate authorities built in, not the system's.

## Not yet looked at by anyone else

No outside review or audit has been done. The parsers that read what
others send (the config, Livestatus requests, HTTP requests, the worker
protocol, check result files) are tested for what they should take, and
have not been fuzzed.
