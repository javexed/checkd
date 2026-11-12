# Installing checkd

Not yet tried on a machine of its own: these steps follow from the tests,
and the unit files pass `systemd-analyze verify`.

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
