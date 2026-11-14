# A service made of others

"The website is up when two of the three web servers and the database
are": a service whose state the engine works out from other hosts and
services. Nothing is run; the check's command says what counts.

    define command {
        command_name  aggregate
        command_line  checkd:aggregate $ARG1$
    }
    define service {
        host_name            site
        service_description  web servers
        check_command        aggregate!--min 2 web*/http
        check_interval       1
        ...
    }
    define service {
        host_name            site
        service_description  website
        check_command        aggregate!'site/web servers' db1/postgres @edge
        ...
    }

A member is

- a host: `web1`, a pattern `web*` (`*` any run of characters, `?` any
  one), or `@group` for the members of a host group;
- a service: `web1/http`, with a pattern or a group in the host's place
  and a pattern for the description: `web*/http`, `@webs/disk *`.
  A name with spaces goes in quotes.

An aggregate can be a member of another, as above.

| | State |
|---|---|
| without `--min` | as bad as the worst member: OK, WARNING, UNKNOWN, CRITICAL in that order |
| `--min 2`, `--min 50%` | OK when all are OK; WARNING when some are not and at least that many are; CRITICAL when fewer are |

- A host that is down or cannot be reached counts as CRITICAL.
- A member counts by its hard state: an attempt that fails says nothing
  until the attempts are used up. One not checked yet counts as OK.
- The text says how many are OK and names up to eight that are not, and
  the performance data has the number: `ok=2;;2:;0;3`.
- A member that names no host or service makes the aggregate UNKNOWN,
  and says which.

The aggregate is worked out when its check is due, like any check, so it
follows its members by up to its `check_interval`: keep that short, it
costs nothing. The rest is as for any service: attempts, notifications,
downtimes, dependencies. To hear of the aggregate and not of each
member, give the members no contacts, or a notification dependency on
nothing less than the aggregate ([digest](digest.md)).

Not there: weights, "best of", rules written as expressions. Naemon does
not know `checkd:aggregate`.
