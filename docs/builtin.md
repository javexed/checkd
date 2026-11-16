# Checks without a plugin

A command line that begins with `checkd:` starts no process: checkd
makes the check itself. For four checks many sites begin with, that
does away with installing plugins, and with a process for every check.

    define command {
        command_name  ping
        command_line  checkd:ping -H $HOSTADDRESS$
    }
    define command {
        command_name  web
        command_line  checkd:http -H $HOSTADDRESS$ -S -u $ARG1$
    }

Everything else about the command is as for a plugin: macros, the
check's timeout, the output with its performance data, the four states.
A remote worker makes the check from where it runs. Naemon does not know
these: a config that uses them is a config for checkd only (Naemon takes
it, and each such check then fails to start).

`checkd check checkd:tcp -H localhost -p 22` makes one check once, says
what it found and ends with its state, for trying a line out.

| Check | Options | OK when |
|---|---|---|
| `checkd:ping` | `-H host`, `-w ms`, `-c ms` | all of three echo requests are answered (a lost one: WARNING; none: CRITICAL), on average within the times |
| `checkd:tcp` | `-H host -p port`, `-w s`, `-c s` | the port takes a connection, within the times |
| `checkd:http` | `-H host`, `-p port` (80, or 443 with `-S`), `-u path` (`/`), `-S` for HTTPS, `-e codes`, `-s text`, `-w s`, `-c s` | the answer is 2xx or 3xx (4xx: WARNING, 5xx: CRITICAL), or one of the codes of `-e`; the text of `-s` is on the page; with `-S`, the certificate is trusted |
| `checkd:cert` | `-H host`, `-p port` (443), `-w days` (21), `-c days` (7) | the certificate is trusted and lasts longer than the days |

- **ping** needs no root: Linux gives a process whose group is within
  `net.ipv4.ping_group_range` a socket for echo requests, and most
  distributions put every group within it. Where not, the check says so
  (UNKNOWN). IPv4 and IPv6.
- **http** asks once with `GET` and follows no redirect: a 3xx is the
  answer, and OK. `-s` reads up to 1 MB of the page.
- **cert** and `http -S` trust the certificate authorities built into
  checkd (the `webpki-roots` list), not the system's; a certificate of
  an authority of your own counts as not trusted. `cert` says how long a
  certificate lasts even when it also says it is not trusted. A build
  without the feature `tls-checks` has neither.
- A name that is not known, a port that is closed and a host that does
  not answer are CRITICAL; a line that cannot be understood is UNKNOWN.

## The machine itself

Five more look at the machine they run on: the engine's own host, or,
sent to an agent ([agent.md](agent.md)), the agent's host. They read
only, and need no plugin on the host.

| Check | Options | OK when |
|---|---|---|
| `checkd:disk` | `-p path` (`C:` on Windows), `-w %`, `-c %` | the filesystem is used no more than the share (as `df` counts it) |
| `checkd:memory` | `-w %`, `-c %` | memory is used no more than the share (what is not available without swapping) |
| `checkd:load` | `-w 1,5,15`, `-c 1,5,15` (or one number for all three) | the load average is within the limits; Linux only |
| `checkd:cpu` | `-w %`, `-c %` | the processors are busy no more than the share, over one second |
| `checkd:service` | `-s name` | the service runs: a systemd unit that is active, or a Windows service that is running; one that is not there is CRITICAL |

Their performance data: `used=` (%), `free=` (bytes) and `free_pct=` (%) for disk, `used=`
and `available=` for memory, `load1=`, `load5=`, `load15=`, and `busy=`.

### A value that wavers at a limit

Disk, memory, load and CPU can keep a state until the value is clearly
back: with `-C`, a WARNING or CRITICAL ends only when the value is under
its limit by the margin, so a load that moves between 3.9 and 4.1 around
a limit of 4 does not change state, and notify, each time it crosses.

    command_line  checkd:load -w $ARG1$ -c $ARG2$ -C 10% -P $SERVICESTATEID$

- `-C 10%` is a share of each limit; `-C 2` an amount in the value's own
  unit (percentage points for disk, memory and CPU).
- `-P` is the state the service is in now, which the check cannot know
  itself: always `$SERVICESTATEID$`. UNKNOWN keeps nothing.
- Rising is as without `-C`: the limit itself, then `max_check_attempts`.
- While the margin keeps a state, the text says until when, as
  `(WARNING until under 15 min 3.6)`.
- The service has one state, so load's three averages share it: once
  raised, it stays until each average is clearly under its own limit,
  whichever raised it. A state the value did not set, such as a CRITICAL
  from a timeout, is kept by the same rule.

An agent older than this ignores `-C` and `-P`. Naemon's flap detection
is another thing: it counts state changes over the last 20 results and
holds back notifications while there are many.

`checkd:aggregate` is one more, of another kind: a service worked out
from other hosts and services ([aggregate](aggregate.md)).

Not there: SNMP, DNS records, authentication, a client certificate,
following redirects, a proxy. For those there are plugins.

## Telling someone without a script

    define command {
        command_name  notify-service-ntfy
        command_line  checkd:notify -f ntfy -u https://ntfy.example.com/alerts -t "$HOSTNAME$/$SERVICEDESC$ is $SERVICESTATE$" -m "$NOTIFICATIONTYPE$: $SERVICEOUTPUT$"
    }

`checkd:notify` is for a notification command (or an event handler): it
sends a title and a message to an address, and starts no program.

| `-f` | What is sent |
|---|---|
| `json` (without `-f`) | `POST` of `{"title": ..., "message": ...}` to the address: for a webhook of your own |
| `ntfy` | to an ntfy server; the address ends with the topic, and `-p 1` to `5` is the priority |
| `slack` | `{"text": "*title*\nmessage"}`, as Slack's incoming webhooks and those made like them take |

- The title and the message are words of the line, in quotes. The
  engine settles the words of a `checkd:` line from what the
  configuration wrote, before it fills in what a check printed: output
  with quotes, spaces or what looks like an option stays inside the word
  its macro stood in, and no shell reads any of it. (`$ARGn$` and
  `$USERn$` are the configuration's, and do make words.)
- Over HTTPS the certificate must be one the built-in authorities vouch
  for. An answer that is not 2xx, or none, is a failure, and in the log
  as one for a notification command.
- The address is in the configuration, and is all a sender needs: who
  can read the configuration can send there.
- No authentication header, no retry: a message that is not taken is
  not sent again.

`checkd:trend` says where a number is going ([trend](trend.md)).
