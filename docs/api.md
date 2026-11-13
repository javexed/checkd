# HTTP API, metrics, and performance data export

All three are off unless asked for.

## HTTP API

    CHECKD_HTTP_TOKEN=... checkd run --http 127.0.0.1:6559 naemon.cfg

| Request | Answer |
|---|---|
| `GET /api/v1/<table>` | the rows of a Livestatus table, as a JSON list of objects |
| `POST /api/v1/command` | carries out the body as one external command |
| `GET` or `POST /api/v1/push/<token>` | a result for the service with that token: [push](push.md) |
| `GET /metrics` | the engine's health in Prometheus' text format |

`<table>` is any table Livestatus answers: `hosts`, `services`, `status`,
`hostgroups`, `servicegroups`, `contactgroups`, `contacts`, `commands`,
`timeperiods`, `comments`, `downtimes`, `log`, `hostsbygroup`,
`servicesbygroup`, `servicesbyhostgroup`, `statehist`, `columns`. Query
parameters:

- `columns=name,state` — the columns to give; without it, a few that the
  table has;
- `filter=state = 2;;host_name = web` — Livestatus filter lines, separated
  by `;;`, all of which must hold;
- `limit=100`.

The answer is the Livestatus answer with its column names as keys, so it
agrees with what Thruk sees.

The body of a command is what a line of the command file would be,
without the time: `ACKNOWLEDGE_SVC_PROBLEM;web;disk;2;1;1;ann;looking`.
Commands go through the same parser and leave the same lines in the log.

With `CHECKD_HTTP_TOKEN` set, every request needs `Authorization: Bearer
<token>` (but a push, which has a token of its own), and commands are
taken. Without it, reads are answered and
commands refused: the API is read-only. The server speaks plain HTTP; put
it behind a proxy that does TLS where the network is not trusted.

## Metrics

`GET /metrics` gives:

| Metric | |
|---|---|
| `checkd_checks_total{kind}` | checks started, for hosts and services |
| `checkd_commands_running` | commands running now |
| `checkd_checks_waiting` | checks planned and not yet started |
| `checkd_check_latency_seconds` | the average latency of the last check of each object |
| `checkd_notifications_total` | notification commands started |
| `checkd_external_commands_total` | external commands taken, by any way in |
| `checkd_livestatus_requests_total` | Livestatus requests answered |
| `checkd_objects{kind,state}` | hosts and services by current state |
| `checkd_store_failing` | 1 while the store cannot be written |

The OpenTelemetry export (`--otlp-endpoint`, in a build with the `otel`
feature) sends checkd's own metrics and traces to a collector instead.

## Performance data export

    checkd run --perfdata-graphite graphite:2003 naemon.cfg
    checkd run --perfdata-influx 'http://influx:8086/write?db=checkd' naemon.cfg

Each result's performance data, read by the plugin guidelines'
`label=value[unit];warn;crit;min;max`, is sent straight to a metrics
store, beside the performance data files or instead of them:

- **Graphite**, plaintext over TCP: `checkd.<host>.<service>.<label>
  <value> <time>`, `_host_` in place of the service for a host's own data,
  and `_` for any character Graphite takes as a separator.
- **InfluxDB**, line protocol posted over HTTP: measurement `checkd`, tags
  `host`, `service` and `label`, fields `value`, `warn`, `crit`, `min`,
  `max` and `unit` where the plugin gave them. `CHECKD_INFLUX_TOKEN` is
  sent as `Authorization: Token ...`. Plain HTTP only.

An object whose `process_perf_data` is 0 sends nothing. Values go in
batches every five seconds; when the store cannot be reached, up to
100,000 lines wait and the oldest are dropped beyond that, and the
failure is said once in the log.

Not done: performance data as OpenTelemetry metrics.
