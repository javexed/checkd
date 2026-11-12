# Remote workers

Checks, event handlers and notification commands can run on other
machines: workers, which take their jobs from the engine. This is what
mod_gearman does for Naemon, with a Gearman job server between them. In
checkd the engine is the job server, and nothing else is installed. Only
in a build with the `workers` feature:

    cargo build --release -p checkd --features workers

## Setting up

1. Certificates. Workers talk to the engine over TLS and trust only the
   certificate authority they are given. A site with its own CA makes the
   engine a certificate for its host name; otherwise:

       checkd make-certs /etc/checkd/workers engine.example.test

   writes `ca.crt`, `ca.key`, `engine.crt` and `engine.key`. Workers need
   `ca.crt` only. Keep `ca.key` somewhere other than the engine's machine.

2. A token per worker, one word in a file of its own:

       head -c 24 /dev/urandom | base64 > /etc/checkd/workers/dmz-1.token

   The worker has a copy. Removing its line from the workers' file and
   restarting the engine shuts it out, without touching the others.

3. The workers' file, given to the engine with `--workers`:

       listen=0.0.0.0:4730
       cert=engine.crt
       key=engine.key
       hostgroups=dmz,lab
       localhostgroups=core
       services=yes
       queue_custom_variable=WORKER
       worker=dmz-1 token=dmz-1.token pools=hostgroup_dmz
       worker=all-1 token=all-1.token pools=service,eventhandler

   File names are read from the file's directory.

4. The engine:

       checkd run --workers /etc/checkd/workers/workers.cfg naemon.cfg

5. Each worker:

       checkd worker --engine engine.example.test:4730 --name dmz-1 \
           --token-file /etc/checkd/dmz-1.token --ca /etc/checkd/ca.crt \
           --restrict-path /usr/lib/nagios/plugins

## Where a job runs

A pool is what mod_gearman calls a queue, and the settings keep
mod_gearman's names. For a check, the first of these that applies:

1. `queue_custom_variable=WORKER`: the object's custom variable `_WORKER`
   names its pool. A service without one takes its host's.
2. `localservicegroups`, `localhostgroups`: services in these service
   groups, and hosts in these host groups with their services, run on the
   engine.
3. `servicegroups=a,b`: services in group `a` go to pool
   `servicegroup_a`.
4. `hostgroups=a,b`: hosts in group `a`, and their services, go to pool
   `hostgroup_a`.
5. `hosts=yes`: other host checks go to pool `host`; `services=yes`:
   other service checks go to pool `service`.
6. Otherwise, the engine runs it.

Event handlers run on the engine, unless `eventhandler=yes` sends them to
pool `eventhandler`, or `route_eventhandler_like_checks=yes` sends them
where their object's checks go. `notifications=yes` sends notification
commands to pool `notification`. Commands that process performance data
files always run on the engine.

A `worker=` line names a worker, the file of its token, and the pools it
may serve. A worker serves all of those it is let.

## What happens when

- **Free slots.** A worker takes as many jobs at once as `--max-jobs`
  says (8). The engine hands each job to a worker of its pool with a free
  slot, taking turns.
- **Nobody serves the pool.** The job gets a result at once: UNKNOWN, or
  what `orphan_return` says (0 to 3), with the output `(No worker is
  connected for pool <pool>)`. A host check that gets UNKNOWN makes the
  host DOWN, as on Naemon.
- **Waiting too long.** A job that waits more than `max-age` seconds (60)
  for a free slot gets such a result too.
- **A worker goes away** (it ends, its machine dies, the network breaks:
  the engine notices at once when the connection is closed, and within 30
  seconds when it just goes quiet). The jobs it held go to another worker
  of their pool, once. A job whose second worker goes away too gets a
  result saying so.
- **A worker does not answer.** A job held ten seconds past its timeout
  is taken as timed out, as it would be on the engine.
- **The connection breaks, on the worker's side.** The worker stops what
  it was running, since the engine gives those jobs to another worker,
  and connects again after a pause that grows to 30 seconds.
- **Failover.** Only the node that leads listens for workers. Give each
  worker both nodes, `--engine a:4730 --engine b:4730`; it tries them in
  turn. In the test below its checks went on 18 seconds after the leader
  was killed.

The engine never runs two checks of one host or service at once, so no
check is out with two workers.

## The worker

    --engine <host:port>   the engine; more than once for failover nodes
    --server-name <name>   the name in the engine's certificate, when it is
                           not the host of --engine
    --name <name>          its name in the workers' file
    --token-file <file>
    --ca <file>
    --max-jobs <n>         jobs at once (8)
    --restrict-path <dir>  run only programs under these directories, and
                           no command line that needs a shell; more than
                           once for more. Anything else is answered with
                           UNKNOWN and the reason.
    --load-limit1 <n>, --load-limit5 <n>, --load-limit15 <n>
                           take no new jobs while the load average over 1,
                           5 or 15 minutes is above n

A worker runs a job as the engine does on its own machine: directly, or
through `/bin/sh` when the line needs a shell, in a process group of its
own that is stopped at the timeout, as the user the worker runs as. The
plugins and their paths must be on the worker's machine.

## Security

- The connection is TLS 1.2 or 1.3, and the worker checks the engine's
  certificate against its CA. The engine does not ask workers for
  certificates; a worker shows who it is with its token.
- The engine sends workers the command lines, with macros filled in:
  host names, addresses, and whatever else the commands carry, such as
  passwords in `$USERn$` macros. A worker should be a machine you would
  let run those commands. `--restrict-path` limits what a worker runs if
  someone else gets hold of the engine's side.
- A worker's token gets jobs only for the pools in its `worker=` line,
  and lets it send results only for the jobs it was given.

## Metrics

`/metrics` (with `--http`) adds, per pool:

| Metric | |
|---|---|
| `checkd_pool_workers{pool}` | workers connected that serve it |
| `checkd_pool_jobs_waiting{pool}` | jobs waiting for a free slot |
| `checkd_pool_jobs_running{pool}` | jobs held by workers |
| `checkd_pool_oldest_waiting_seconds{pool}` | how long the oldest job has waited |

## Compared with mod_gearman

- No Gearman job server, and no NEB module: the engine is both.
- TLS and a token per worker, in place of one AES key shared by all.
- The same routing settings, by name: `hosts`, `services`,
  `eventhandler`, `route_eventhandler_like_checks`, `notifications`,
  `hostgroups`, `servicegroups`, `localhostgroups`, `localservicegroups`,
  `queue_custom_variable`, `orphan_return`, `max-age`. Defaults are
  checkd's: everything off, `orphan_return=3`, `max-age=60`. Other
  settings of mod_gearman are not read, and a file with one is refused.
- Pool names follow mod_gearman's queue names (`hostgroup_<name>`,
  `servicegroup_<name>`, `host`, `service`, `eventhandler`,
  `notification`), so dashboards that name queues carry over.
- Not done: performance data through workers (mod_gearman's `perfdata`
  queue), workers that check their results' age, and `checkd worker` as
  a service file.

## Tests

`runs/workers/run` starts an engine and three workers in containers on an
internal network that reaches nothing outside, and checks: routing by
host group, kept-local group, custom variable and default pool; UNKNOWN
for a pool nobody serves; a program outside `--restrict-path` refused; a
worker with a wrong token refused; the pools in `/metrics`; and a worker
killed half way through a 15-second check, after which the other worker
of its pool runs it again. Unit tests in `crates/checkd/src/workers.rs`
cover the queueing.

`runs/workers/failover` runs two nodes in failover and a worker that is
given both: the worker joins the leader and not the standby, and when the
leader is killed it finds the new one by itself and its checks go on.
