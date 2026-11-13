# Results pushed to checkd, and the alarm when none comes

For what runs elsewhere and on its own time: a backup, a cron job, a
script on a machine checkd cannot reach. The job tells checkd when it is
done; checkd raises the alarm when it has not heard in time.

    define service {
        host_name              nas
        service_description    backup
        check_command          ok            ; never run: see below
        active_checks_enabled  0
        max_check_attempts     1
        _PUSH_TOKEN            8f3c1e0b7a9d4c52b6e1
        _PUSH_EVERY            90000         ; seconds: a day and an hour
    }

and, at the end of the job:

    curl -fsS https://checkd.example.com/api/v1/push/8f3c1e0b7a9d4c52b6e1

- `GET` or `POST /api/v1/push/<token>` is a result for the service whose
  `_PUSH_TOKEN` it is: OK, with the text "pushed". `?state=warning` (or
  `critical`, `unknown`, `0` to `3`) and `?output=...` say otherwise; the
  body of a `POST` is the text when there is no `output`. The first line
  is kept, up to 1,000 characters.
- With `_PUSH_EVERY <seconds>`, the service goes CRITICAL, "nothing was
  pushed for ... seconds", when that long has gone by since the last
  push, and again each time it goes by. The time counts from the start
  of checkd, not from before it: after a restart a job has its whole
  span. It is looked at every five seconds.
- Either way the service gets a passive result, by the external command
  `PROCESS_SERVICE_CHECK_RESULT`: it is in the log as one, and attempts,
  notifications, acknowledgements and downtimes are as for any service.
  The service must take passive results (`passive_checks_enabled`, on
  unless turned off), and the engine too (`accept_passive_service_checks`).

Needs `--http <address>` ([api](api.md)); checkd says at its start when
services have tokens and it does not listen. The token is the whole of
what a caller needs: a push asks for no bearer token. So:

- A token is at least 16 letters, digits, `-` and `_`; a shorter one is
  refused at the start, with a warning. Make them at random
  (`openssl rand -hex 16`), one for each service.
- Who knows a token can set that one service to any state, and nothing
  else. A token that is not known is answered 404, like any other path.
- A token is a custom variable: who can read the configuration, or the
  custom variables over Livestatus, can read it.
- The API speaks plain HTTP. Across a network that is not trusted, put it
  behind a proxy that does TLS, or the token is read on the way.

Hosts take no pushes, only services.
