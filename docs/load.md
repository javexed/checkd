# Load

`runs/load/run [hosts] [services per host] [interval s] [seconds]` runs
Naemon Core 1.5.2 and checkd, one after the other, on the same generated
config in the oracle image: every service checked by a no-op plugin (a
shell script that adds a byte to a file, so that the calls can be
counted) every `interval` seconds, host checks off, no cache horizon. It
says, per engine, the checks run a second, the average and largest check
latency, how many services were more than two intervals behind at the
end, and the peak memory of the engine's own process.

## Results

On one machine with 8 cores and 31 GB, 2026-11-12, measured over 60 s:

| Services | Asked a second | Naemon: run a second | Naemon: latency avg / max | Naemon: behind | Naemon: memory | checkd: run a second | checkd: latency avg / max | checkd: behind | checkd: memory |
|---|---|---|---|---|---|---|---|---|---|
| 10,000 | 1,000 | 1,000 | 1.7 ms / 16 ms | 0 | 24 MB | 1,000 | 1.0 ms / 2 ms | 0 | 68 MB |
| 20,000 | 2,000 | 2,000 | 6.5 ms / 43 ms | 0 | 42 MB | 2,000 | 0.9 ms / 2 ms | 0 | 130 MB |
| 40,000 | 4,000 | 3,999 | 4.3 ms / 45 ms | 0 | 78 MB | 4,000 | 1.0 ms / 3 ms | 0 | 254 MB |
| 40,000 | 8,000 | 7,546 | 52 ms / 0.6 s | 5,786 | 94 MB | 8,001 | 1.0 ms / 4 ms | 0 | 255 MB |
| 40,000 | 13,333 | 7,978 | 0.16 s / 1.3 s | 18,696 | 120 MB | 13,331 | 0.6 ms / 7 ms | 0 | 255 MB |
| 80,000 | 16,000 | 7,448 | 1.7 s / 2.9 s | 40,746 | 198 MB | 15,275 | 0.6 ms / 6 ms | 0 | 518 MB |

Loading a config of 80,000 services (`checkd verify`): 1.1 s; Naemon's
`naemon -v`: 0.4 s.

- checkd keeps up to about 15,000 checks a second here; Naemon to about
  7,500, beyond which its checks fall behind.
- Memory. The table is from before two changes (below). Measured
  again since, 2026-11-13, once the configuration is loaded:

  | Services | Checks a second | Naemon | checkd | checkd before |
  |---|---|---|---|---|
  | 40,000 | 4,000 | 78 MB | 97 MB | 204 MB |
  | 80,000 | 4,000 | 151 MB | 134 MB | |
  | 80,000 | 16,000 asked, 14,000 run | 190 MB | 340 to 380 MB | 393 MB |

  So about 1.7 to 2.4 KB a service at a rate checkd keeps up with
  easily, about what Naemon takes. Near the most it can run, checkd
  takes as much as before the changes: that memory goes with the rate,
  not with the configuration, and where it is was not found out (not in
  the allocator's pools for the threads that start commands: holding
  those to four changed nothing). While it loads, checkd takes more for
  a moment: 253 MB at 40,000 services, 440 MB at 80,000. In the load
  test every service has the same settings, which is the best case for
  the first change.
- CPU time is not compared: checkd starts plugins from threads of its own
  process, Naemon from separate worker processes.

## What the load test found, and what was changed

- **Starting processes on the engine's one thread.** Starting a process
  holds the thread until the process runs its program, longer the more
  processes there are; checkd fell to about 400 checks a second at 8,000
  asked. Processes are now started from a pool of 32 threads.
- **A config loader quadratic in hosts × services.** Each service's host
  list was looked up in a fresh list of all hosts: 80,000 services took
  46 s to load. Now about linear.
- **Memory for checks waiting to start.** Each held two 8 KB read
  buffers from the moment it was asked for, so a backlog under overload
  cost over 1 GB. The buffers are now made when the command runs.
- **The configuration kept twice.** Every host and service had its
  settings once as the configuration gave them and once more in the
  engine, about half a kilobyte each. Objects with the same settings now
  hold one copy, shared by the configuration and the engine; a command
  that changes a setting gives that one object a copy of its own. With
  a configuration of 40,000 like services loaded and no check run yet,
  the process went from 3.6 KB to 2.7 KB a service
  (`memory_per_service` in `crates/checkd-core/tests/memory.rs`). Of
  those 2.7 KB, 1.6 KB is what the configuration and the engine hold;
  the rest is gaps that loading leaves between what stays, too small to
  give back.
- **What loading leaves behind.** Loading frees much more than it
  keeps, and what it keeps lay scattered among what it freed, in tens
  of thousands of gaps too small to give back: with 40,000 services, 68
  MB in use held 168 MB. The configuration is now loaded on a thread of
  its own and copied, close together, to where the engine keeps it; the
  thread's memory, then all free, goes back
  (`checkd_config::load_compact`). The same 40,000 services, loaded and
  not yet under load: 65 MB.
- **The cache horizon.** With the default `cached_service_check_horizon`
  (15 s) longer than the check interval, both engines run every other
  check only; the load test sets it to 0.

## Not measured

A real config, real plugins, notifications, Livestatus queries under
load, the stores' write load, remote workers.
