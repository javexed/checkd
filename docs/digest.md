# Fewer notifications for one cause

**Experimental.** Digests, and their settings, may change before 1.0.

Two things keep one fault from being told many times: dependencies,
which Naemon has too, and digests, which are checkd's own.

## Notification dependencies

A `servicedependency` or `hostdependency` with
`notification_failure_criteria` keeps the dependent from notifying of a
problem while the master is in one of the states named: the log says
"Notification blocked due to dependency of another object." once, and
the notification goes out at the dependent's next check after the master
has left that state, if the problem is still there. `inherits_parent`
and `dependency_period` count as for execution dependencies.

This follows a recording of the reference
(`runs/scenarios/notification-dependencies`). Not seen there, and so not
known to agree: where this reason stands among the others when several
hold, and whether a dependency holds back a recovery (checkd lets
recoveries through). The reference also checks the master again when a
dependent's result comes in; checkd does not.

A service does not notify while its host is down, with or without a
dependency.

## Digests

    define contact {
        contact_name                  ann
        ...
        service_notification_commands notify-by-mail
        host_notification_commands    notify-by-mail
        _CHECKD_DIGEST_COMMAND        digest-by-mail
        _CHECKD_DIGEST_WAIT           30
    }

A contact with `_CHECKD_DIGEST_COMMAND` gets its notifications a little late.
The first is held for `_CHECKD_DIGEST_WAIT` seconds (30 without it; 1 to 3600),
and whatever else comes for the contact in that time is held with it.

- One alone then goes out as it is, by its own command.
- Two or more go out as **one** run of the digest command, and their own
  commands are not run. The log has each notification as always, and one
  line `NOTIFICATION DIGEST: <contact>;<how many>;<summary>`.

The digest command is told what it is about in its environment:

| Variable | |
|---|---|
| `CHECKD_DIGEST_CONTACT` | the contact |
| `CHECKD_DIGEST_COUNT` | how many notifications |
| `CHECKD_DIGEST_SUMMARY` | `3 PROBLEM, 1 RECOVERY` |
| `CHECKD_DIGEST_LINES` | one line for each: `PROBLEM: web/disk is CRITICAL: <output>` |

Its command line may use `$CONTACTNAME$`, `$CONTACTALIAS$`,
`$CONTACTEMAIL$`, `$CONTACTPAGER$` and `$USERn$`, and no other macro: it
is about no one host or service. Plugin output reaches it only through
the environment, so that nothing in it is read by a shell as a command.

    define command {
        command_name digest-by-mail
        command_line /bin/sh -c 'printf "%s\n" "$CHECKD_DIGEST_LINES" | mail -s "checkd: $CHECKD_DIGEST_SUMMARY" $CONTACTEMAIL$'
    }

(A `;` starts a comment in an object file: a command line cannot have
one.)

What to know:

- Every notification to such a contact is late by up to the wait, a
  single one too.
- Notifications held when checkd stops or reads its configuration again
  go out then, without waiting the rest of the wait; checkd waits up to
  a minute for their commands to end.
- The engine counts each held notification as sent when the digest goes
  out, whether or not the digest command succeeds.
- A digest's command runs on the engine's machine, also where the
  notifications themselves would have gone to a remote worker.
- Naemon takes the two custom variables and does nothing with them.
