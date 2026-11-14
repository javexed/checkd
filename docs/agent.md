# checkd-agent

`checkd-agent` runs one host's checks on that host. It is the remote
worker of [workers.md](workers.md) built on its own, for Linux and Windows:
it connects out to the engine over TLS, says who it is with a token, and
runs the jobs it gets. Nothing listens on the monitored host, and the
engine needs no SSH access to it.

Through an HTTPS proxy or tunnel, the agent reaches the engine as a
WebSocket (`--engine wss://checkd.example.com/workers`, with the engine's
`websocket=` listener behind it); see [workers.md](workers.md).

NRPE keeps working beside it: a service with `check_nrpe` runs on the
engine as before. An agent is an alternative for hosts where an outgoing
TLS connection is easier than an incoming NRPE port.

## Where its jobs come from

An agent is named after its host. The engine sends a host's **service**
checks to the worker whose pool is `host_<host name>`. The host's own check
stays where it was, usually on the engine, so a host whose agent has
stopped still shows up or down. While no agent of a host is connected, its
services get `orphan_return` (UNKNOWN by default), with an output that
names the pool.

The rule comes after `queue_custom_variable` and before the group rules of
[workers.md](workers.md#where-a-job-runs).

## The engine's side

In the workers' file:

    agents=agents

Each `<host>.token` file in that directory (relative to the workers' file)
is an agent: the worker `<host>` with the pool `host_<host>`. Host names
are letters, digits, `.`, `-` and `_`. checkd reads an agent's token when
the agent connects, and every few seconds while it stays: deleting or
replacing the file lets that agent go at once, and leaves the others
alone. Which hosts have an agent, for routing their checks, is read at
start and at each reload (`SIGHUP` or `RESTART_PROGRAM`). A token file
checkd cannot read is left out with a warning; it does not stop the
engine. checkd needs to read the files: run it as the user that writes
them, or give it their group.

    head -c 24 /dev/urandom | base64 | tr -d '=+/\n' > agents/web1.token
    kill -HUP $(pidof checkd)

A `<host>.new` file is a token not in use yet: checkd lets its agent in,
in the pool `waiting_<host>`, which no check goes to, so the host's checks
stay where they were. nuthatch's Agents page writes one, prints the command
that installs the agent, and once the agent has connected renames it to
`<host>.token` and reloads checkd.

## Building

    cargo build --release -p checkd-agent

or, for Linux (x86_64 and aarch64, static) and Windows (x86_64) at once,
in a container so that nothing is installed here:

    scripts/build-agents.sh

The binaries and their `SHA256SUMS` land in `dist/agent/`.

## On the host

The options are those of `checkd worker`:

    checkd-agent --engine engine.example.test:4730 --name web1 \
        --token-file /etc/checkd-agent/token --ca /etc/checkd-agent/ca.crt \
        --restrict-path /usr/lib/nagios/plugins

What the engine sends an agent is its host's service command lines,
with their arguments and `$USERn$` values filled in: a password or SNMP
community in a check goes to that host. Keep such checks on hosts
without an agent, or with NRPE.

`--restrict-path` is worth giving: without it the agent runs any command
line the engine sends, as its own user.

On Linux it runs as a systemd service under a user of its own. On Windows
it runs as a scheduled task at startup, as `LOCAL SERVICE`.

## On Windows

Not yet tried on a Windows machine: `runs/agent-windows/run` runs the
Windows build under Wine against a real engine.

- Command lines are read as on Linux, so `\` escapes the next character.
  Write paths with `/` (`"C:/Program Files/checkd/check_disk.exe"`) or in
  single quotes (`'C:\Program Files\checkd\check_disk.exe'`).
  `--restrict-path` takes either slash and ignores case.
- A command line that needs a shell runs under `cmd.exe /d /s /c`.
- At its timeout a command is stopped with everything it started
  (`taskkill /t`).
- `checkd:ping` is not available; `checkd:tcp`, `checkd:http` and
  `checkd:cert` are.
- `--load-limit*` has no effect: Windows has no load average.
