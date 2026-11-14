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
