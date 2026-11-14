# Downtimes for many, and downtimes that come round

Both are checkd's own; Naemon has neither. The downtimes they make are
ordinary ones: they are in the log, in Livestatus and in the store, and
can be removed one by one like any other.

## For everything that fits a pattern

    SCHEDULE_MATCHING_DOWNTIME;<hosts>;<services>;<start>;<end>;<author>;<comment>

an external command, by the command file, Livestatus or the HTTP API.

- `<hosts>` is a pattern for host names, in which `*` stands for any run
  of characters and `?` for any one: `web*`, `*-db-?`, `*`. Or `@group`,
  for the members of a host group.
- With `<services>` empty, the hosts that fit get a downtime. Otherwise
  the services on those hosts whose descriptions fit `<services>` do,
  and the hosts do not: `*;backup;...` is every service named backup.
- The downtimes are fixed, from `<start>` to `<end>`, in seconds since
  1970.

It counts for the hosts and services there are when it is given: one
added later is not in the downtime.

## Whenever a timeperiod holds

    define host {
        host_name          nas
        ...
        _CHECKD_DOWNTIME_PERIOD   backup-window
    }
    define timeperiod {
        timeperiod_name    backup-window
        alias              the nightly backup
        sunday             02:00-04:00
    }

A host or service with `_CHECKD_DOWNTIME_PERIOD <timeperiod>` is in a downtime
whenever the timeperiod holds: checkd schedules one, by the usual
command, within a quarter of a minute of its beginning, to the minute
it ends; author `checkd`, comment `recurring: <timeperiod>`. A service
does not inherit its host's.

- A timeperiod that holds for more than seven days gets a downtime of
  seven days, and the next one after it.
- After a restart the downtime that runs is known again by its author
  and comment, and not made twice.
- Removing such a downtime while its timeperiod still holds brings it
  back within a quarter of a minute. To end it early, change the
  timeperiod or take the variable away.
- A variable that names no timeperiod gives a warning at the start, and
  no downtime.
