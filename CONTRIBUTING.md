# Contributing

## Clean-room rule

checkd is licensed MIT OR Apache-2.0. Naemon and Nagios Core are GPL-2.0, so
nothing in this repo may be derived from their source.

- Do not port, translate or copy from Naemon or Nagios source, and do not open
  that source while working on checkd.
- Take behaviour from the public documentation and from black-box tests
  against a running Naemon.
- Write sample configs, test fixtures and doc text for this project. Do not
  copy them from Naemon or Nagios.
- Naemon is a test oracle. The test harness installs it as a separate binary;
  it is never vendored into this repo.
- Settle edge cases by experiment against the oracle, and record the
  experiment as a test.
- Config trees that other people published are local test input only
  (`real/`). They are not committed, and nothing is copied from them. What
  one shows goes into the corpus as a case written for this project.
- The same goes for the config files that the Naemon package installs and
  for the config of an OMD site (`real/stock`, `real/omd`). They belong to
  those projects and are config files, not source. A setting that the
  loader first met there is known to it by name only. What the setting
  does was taken from the oracle, with configs written for this project.

- The engine's behaviour (phase 2 on) is taken the same way: a scenario
  written for this project is run on the reference daemon with scripted
  plugins (`oracle/play`), and its log and plugin calls are kept beside it
  in `runs/scenarios/<name>/expected`. Where checkd does something else on
  purpose, the scenario's `compare` file says so and why.

- The same for the reference's state file, `retention.dat`: its block
  kinds and keys were read off files the reference wrote at the end of
  runs on configs written for this project. The import reads that
  format; no Naemon code that writes or reads it was looked at.
- Test tools that are pulled as images (the OpenTelemetry collector,
  PostgreSQL) are pinned by digest in the script that uses them, run
  without a network, and are no part of checkd.

- Thruk is GPL too, and is no part of checkd. How it picks the columns it
  asks a backend for was read in its source, to make checkd's version
  string one it treats as Naemon's; nothing of it is copied. What Thruk
  sends and renders (`livestatus/recorded`, `livestatus/rendered`, the
  queries of `runs/scenarios/livestatus-thruk`) stays out of git.

By contributing you confirm that your change was not written from the GPL
source.

## AI-written code

Much of the code is written by Claude, which may have seen Naemon or Nagios
source in its training data. The rule above still applies to how the work is
done. Whether that supports a clean-room claim is a legal question that has
not been checked.
