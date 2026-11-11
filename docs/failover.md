# Failover

Two checkd nodes share one PostgreSQL database. One leads and does all the
work; the other stands by and takes over when the leader is gone. A site
that runs one node needs none of this.

## Running it

On each node, the same configuration and plugins, and:

    checkd run --failover a --peer b:6558 \
        --store 'postgres:host=db.example user=checkd dbname=checkd' naemon.cfg
    checkd run --failover b --peer a:6558 \
        --store 'postgres:host=db.example user=checkd dbname=checkd' naemon.cfg

under a supervisor that starts checkd again whenever it ends with any
status other than 0: systemd's `Restart=on-failure`, for one. Status 3
means the node lost the lease; status 2 that it could not start, as when
the database cannot be reached. A node always starts as a standby.

| Option | Default | |
|---|---|---|
| `--failover <node>` | | this node's name |
| `--peer <addr>` | | where the other node answers the heartbeat |
| `--peer-listen <addr>` | `0.0.0.0:6558` | where this node answers it |
| `--livestatus-tcp <addr>` | `0.0.0.0:6557` | Livestatus over TCP, for the other node to forward to |
| `--advertise <host>` | the node's name | the name the other node reaches this one by |
| `--lease <seconds>` | 15 | how long the lease lasts unrenewed |
| `--isolated keep\|hold` | `keep` | what an isolated leader does with notifications |

It needs a build with the `postgres` feature. The database is reached
with TLS unless it is on the same machine; on a network the site trusts,
`CHECKD_STORE_ALLOW_PLAIN=1` allows `sslmode=disable`.

## How it works

- **The lease** is a row in the database: who holds it, a counter that
  goes up at each takeover, and when it runs out, by the database's clock.
  The leader renews it every third of its length, from a thread of its
  own that waits at most 5 seconds for an answer.
- **A standby** takes the lease once it has run out, and only after it has
  been connected to the database for a full lease period itself: after an
  outage, the leader has time to renew first. Then it reads the state from
  the database and starts checking. Until then it forwards Livestatus
  connections, lines of the command file and check result files to the
  leader, so Thruk and senders can point at either node.
- **Fencing.** Every write to the database is made only while the writer
  holds the lease with the counter it took it with. A write that is
  refused, or a renewal that finds the lease moved on, stops the node at
  once: it logs it and ends with status 3.
- **The heartbeat.** Each node answers on TCP whether it leads, and with
  which counter. A leader that cannot reach the database asks the other
  node:
  - if that one leads with a newer counter, the leader stops;
  - if it answers at all, it can take the lease once it runs out, so the
    leader stops by itself before that: after 60% of the lease without a
    renewal, by its own clock. A leader's writes then wait at most 2
    seconds, so that nothing holds it up;
  - if it cannot be reached either, the leader cannot tell. It goes on
    and notifies (`keep`, the default: a duplicate alert costs less than
    a missed one), or holds notifications back (`hold`) until the
    database answers again.
- Each node writes a hash of its resolved configuration in the database,
  and logs it when the other node's differs.

## What was tested

`runs/failover/run`: a PostgreSQL server and two nodes in containers on
two internal networks, one to the database and one between the nodes,
with a lease of 15 seconds. Two runs, both with every phase as it should
be:

| Phase | What happened |
|---|---|
| A command and a Livestatus query sent to the standby | reached the leader |
| The leader killed | the standby took over after 14 s and checked 0.2 s later: 14 s from the loss to the first check, against a target of a minute |
| The leader cut off from the database | it stopped after 10 s; the other took over after 15 s |
| The nodes cut off from each other, both reaching the database | the lease stayed where it was |
| The leader cut off from everything | it went on, as decided; the other took over; it stopped 1 and 8 s after it was back |

Through all of it, no two nodes held the lease at once, but in the last
phase, where the design allows it; and no node ran a check while it did
not hold the lease.

## Not done

- A journal on disk for writes made while the database cannot be
  reached: they wait in memory, and are lost if the leader stops before
  the database is back. The design's rule for a stale journal is so far
  met by there being none.
- Forwarding by the standby of commands that come in while no leader is
  known: they are logged and dropped.
- The config hash as a metric.
- Remote workers following the leader (phase 6).
