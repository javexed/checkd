# Reading the configuration again

    kill -HUP <pid of checkd>

or the external command `RESTART_PROGRAM`, by the command file, Livestatus
or the HTTP API. checkd then reads its configuration again, in the same
process, as Naemon does on SIGHUP.

- **A configuration that cannot be used is not taken.** checkd first reads
  the new configuration; when it has errors, it writes them to its log
  (`... is not taken; the one that runs stays`) and goes on as it was.
  `checkd verify` says the same beforehand.
- **State is kept.** States, attempts, acknowledgements, downtimes,
  comments and what commands switched off go to the store and are read
  back, as over a restart; without a store they are carried over in
  memory. A host or service that is no longer configured loses its state
  and its comments and downtimes; a new one starts as pending.
- **History is kept** with a store: the Livestatus log table and the state
  history start from the store's log again.
- **What is interrupted.** Checks that were running are stopped and run
  again on their next turn. The Livestatus socket, the HTTP API and the
  workers' listener close and open again within the moment; a client
  asking in that moment gets no answer and asks again. Remote workers
  connect again by themselves.
- **What is not read again.** The command file stays the one the first
  configuration named. The options of the command line stay as they were.
- **Not with failover.** A node started with `--failover` does not read
  its configuration again; it says so in its log. Restart the standby
  with the new configuration, then the leader.

`runs/reload/run` adds a host and reloads, breaks the configuration and
reloads, and takes a service away and restarts by command, with an
SQLite store and with none.
