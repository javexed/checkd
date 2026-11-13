# Installing checkd

`runs/install/run` follows the system service's steps below, line for
line, in a container that runs systemd and nothing else, and then looks:
the service runs as its user, checks and answers Livestatus, reloads,
refuses to reload a config with errors and goes on, keeps its state over
a stop and a start, and is started again when killed. The user service
at the end runs on a desktop machine. Both unit files pass
`systemd-analyze verify`. Not tried: the worker's unit, failover under
systemd, and a distribution other than Arch Linux.

## Build

    cargo build --release -p checkd --features postgres,workers

Leave out the features you do not need: `postgres` for a PostgreSQL store
and failover, `workers` for remote workers, `otel` for OpenTelemetry. The
binary is `target/release/checkd`.

## Install

    install -m 755 target/release/checkd /usr/local/bin/checkd
    useradd --system --home /var/lib/checkd --shell /usr/sbin/nologin checkd
    install -d -o checkd /etc/checkd
    # the main config and the object files in /etc/checkd; the plugins
    # where the config's commands say
    checkd verify /etc/checkd/naemon.cfg
    install -m 644 contrib/systemd/checkd.service /etc/systemd/system/
    systemctl daemon-reload
    systemctl enable --now checkd

In the main config, paths that suit the unit: `log_file` and
`state_retention_file` under `/var/lib/checkd` or `/var/log/checkd`,
`command_file` and the Livestatus socket of the `broker_module` line
under `/run/checkd`. Without `--store`, the state is kept in
`checkd.sqlite` beside `state_retention_file`.

- `systemctl reload checkd` checks the config first, and reads it again
  only when it can be used ([reload](reload.md)).
- `systemctl stop checkd` saves the state.
- A Naemon site moving over: on its first start on an empty store,
  checkd reads Naemon's `retention.dat` ([store](store.md)).

## A remote worker

`contrib/systemd/checkd-worker.service`, on the worker's machine, with
the engine's address, the CA certificate and the worker's token
([workers](workers.md)).

## Failover

Each node runs the unit with `--failover <node> --peer <other>:6558
--store postgres:...` added to `ExecStart`; the unit restarts checkd
when it ends after losing the lease ([failover](failover.md)).

## Without root: a user service

For one machine watched from a desktop account. Binary in
`~/.local/bin`, config in `~/.config/checkd`, state in
`~/.local/state/checkd` (`StateDirectory=` of a user unit):

    install -m 755 target/release/checkd ~/.local/bin/checkd
    install -m 644 contrib/systemd/checkd-user.service \
        ~/.config/systemd/user/checkd.service
    systemctl --user daemon-reload
    systemctl --user enable --now checkd
    loginctl enable-linger "$USER"    # keep it running when logged out

The unit gives the Livestatus socket on the command line,
`$XDG_RUNTIME_DIR/checkd/live`; the config's `command_file` and
`lock_file` go under `$XDG_RUNTIME_DIR/checkd` too. Plugins run as the
user, so a check that needs root (`check_icmp` without its capability,
say) does not work here. Notification commands that reach the desktop
need the session bus, for example
`/usr/bin/env DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus /usr/bin/notify-send ...`.
