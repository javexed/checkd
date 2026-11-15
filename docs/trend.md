# Where a number is going

"The disk is full in four days", not only "the disk is at 90%": a
service that watches a number of another service's performance data and
says when it reaches its limit, if it goes on as it has.

    define command {
        command_name  trend
        command_line  checkd:trend $ARG1$
    }
    define service {
        host_name            nas
        service_description  Disk / in time
        check_command        trend!'nas/Disk /' / --warn 7 --crit 2
        check_interval       5
        ...
    }

    checkd:trend <host>/<service> <label> [--full <n>] [--warn <days>] [--crit <days>] [--over <hours>]

- `<host>/<service>` is the service whose number it is, and `<label>`
  the number's name in its performance data (`/=4096MB;...` is `/`).
  Names with spaces go in quotes.
- Full is the maximum the performance data names (`...;0;10240`), or
  `--full`.
- WARNING when full is less than `--warn` days away (7), CRITICAL when
  less than `--crit` (2), and CRITICAL when it is full.
- What was read is kept for `--over` hours (24; up to 60 days), and the
  growth a day is that of the straight line that fits it best, so one
  odd reading does not decide it.

Nothing is run: each time the check is due the engine reads the other
service's last result, once for each new result. So:

- For the first half hour it says only what the number is. A number that
  does not grow is OK.
- What was read is kept in the store, so a restart or a reload goes on
  where it was. With `--store none` it starts over each time. A trend
  whose line changes (another label, another `--over`) is a new trend.
- It sees what the other service's checks see: a `check_interval` much
  shorter than that service's gains nothing.
- A number that falls and rises by the hour, like load, is not what this
  is for; a disk, a table, a queue that only grows is.

Naemon does not know `checkd:trend`.
