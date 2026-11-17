# Passive results over HTTP, and the alarm when none comes

For what runs elsewhere and on its own time: a backup, a cron job, a
script on a machine checkd cannot reach. The job tells checkd when it is
done; checkd raises the alarm when it has not heard in time.

    define command {
        command_name   nothing-received
        command_line   checkd:dummy 2 "nothing came in time"
    }
    define service {
        host_name              nas
        service_description    backup
        check_command          nothing-received
        active_checks_enabled  0
        max_check_attempts     1
        check_freshness        1
        freshness_threshold    90000         ; seconds: a day and an hour
        _CHECKD_PASSIVE_TOKEN     8f3c1e0b7a9d4c52b6e1
    }

and, at the end of the job:

    curl -fsS https://checkd.example.com/api/v1/passive/8f3c1e0b7a9d4c52b6e1

- `GET` or `POST /api/v1/passive/<token>` is a result for the service whose
  `_CHECKD_PASSIVE_TOKEN` it is: OK, with the text "received". `?state=warning`
  (or `critical`, `unknown`, `0` to `3`) and `?output=...` say
  otherwise; the body of a `POST` is the text when there is no `output`.
  The first line is kept, up to 1,000 characters.
- The service gets a passive result, by the external command
  `PROCESS_SERVICE_CHECK_RESULT`: it is in the log as one, and attempts,
  notifications, acknowledgements and downtimes are as for any service.
  The service must take passive results (`passive_checks_enabled`, on
  unless turned off; checkd warns at its start when it is off), and the
  engine too (`accept_passive_service_checks`).
- That nothing came in time is Naemon's own freshness checking, which
  checkd does as Naemon does it: when the last result is older than
  `freshness_threshold` seconds, the service's check command is run, also
  with active checks off; here it answers CRITICAL. The engine looks every
  `service_freshness_check_interval` seconds (60 unless set), and the
  time counts from the last result, or from the start of checkd for a
  service that has had none. A config written this way means the same to
  Naemon, but for the `checkd:dummy` command and the token.

Needs `--http <address>` ([api](api.md)); checkd says at its start when
services have tokens and it does not listen. The token is the whole of
what a caller needs: a passive result asks for no bearer token. So:

- A token is at least 16 letters, digits, `-` and `_`; a shorter one is
  refused at the start, with a warning. Make them at random
  (`openssl rand -hex 16`), one for each service.
- Who knows a token can set that one service to any state, and nothing
  else. A token that is not known is answered 404, like any other path.
- With `check_external_commands=0` every passive result is answered 409 and taken
  nowhere; the log says so at the start.
- A token is a custom variable: who can read the configuration, or the
  custom variables over Livestatus, can read it.
- The API speaks plain HTTP. Across a network that is not trusted, put it
  behind a proxy that does TLS, or the token is read on the way.

Hosts take no passive results over HTTP, only services.
