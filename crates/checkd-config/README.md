# checkd-config

Reads a Naemon or Nagios object config: the main config file, the object
files it names, templates, groups, and the lists that stand for many
objects. The result is either a set of resolved objects or a list of
errors.

It was written from Naemon's public documentation and from what the
reference Naemon does with the configs in [`../../corpus`](../../corpus).
No Naemon or Nagios source was read. See
[`../../CONTRIBUTING.md`](../../CONTRIBUTING.md).

## What it is tested against

`cargo test -p checkd-config` loads every case in the corpus and compares
it with the oracle's answer for that case:

- **Accept or refuse.** checkd has to accept a config when Naemon exits 0
  and refuse it when Naemon exits 1. Seven cases are listed in the test as
  refused on purpose: for those, Naemon has to accept, checkd has to
  refuse, and checkd's error has to be the expected one. See "Where checkd
  refuses what Naemon accepts".
- **The objects.** When both accept, checkd's dump has to hold the same
  objects with the same values as Naemon's `objects.precache`.

`CHECKD_CASE=<name> cargo test -p checkd-config` runs one case.

The comparison leaves three things out on purpose:

- **The wording of errors and warnings.** checkd has its own. Only
  accepting or refusing is compared, so checkd can refuse a config for a
  different fault than the one Naemon named. Two cases with more than one
  fault do that: `bad-service-name-only` and `bad-two-faults`.
- **Order where it carries no meaning.** Group members, contacts, contact
  groups, parents, notification commands and `exclude` are compared as
  sets. Dependencies and escalations are compared without regard to the
  order of the entries. Custom variables are sorted first.
- **`initial_state`.** Naemon's dump writes `o` whatever was set.

A second test checks 28 warnings, each at its line. Every kind of warning
in the table further down has at least one.

## Where checkd copies a doubtful Naemon behaviour

In two places Naemon 1.5.2 does something that looks like a fault. checkd
does the same, so that the same objects come out, and warns.

- **`!` in a host escalation or host dependency.** A `!host` item in
  `host_name` or `dependent_host_name` has no effect unless the host group
  setting beside it is given too. `host_name alpha,beta,!beta` still
  includes beta.
- **A host pattern in the members of a service group.** With regular
  expressions on, a pair whose host half is a pattern takes every service
  on the matching hosts. The description half is not used.

Both are a choice that can be reversed: honouring the `!` or the
description would be the more useful behaviour, at the price of objects
that differ from Naemon's for the same config.

## Warnings that Naemon does not give

Naemon accepts each of these without a word. checkd accepts them too and
warns.

| Warning | When |
| --- | --- |
| A setting is given twice | The same setting, or a day of a timeperiod, twice in one definition. The later line is used. |
| A setting has no value | A setting name with nothing after it. It is kept as an empty value. |
| A template uses itself | Directly or through other templates. The loop adds nothing. |
| A dependency is dropped | It names no failure states. |
| A service applies to no host | Its host group is empty, or `!` removed every host. |
| An escalation has no range | Neither `first_notification` nor `last_notification`. |
| An escalation covers no notification | `first_notification` is after `last_notification`. |
| A host has no `check_command` | It is never checked. |
| No hosts are defined | |
| No object files are named | The main config has no `cfg_file` or `cfg_dir`. |
| A value is not a plain number | Something follows the digits, or there are no digits: `3abc`, `3 4`, `often`. The warning names the number Naemon reads, and checkd uses that number. |
| A time is not written HH:MM-HH:MM | Minutes above 59, a single digit for the minutes, seconds, extra zeros. The warning names the times Naemon reads, and checkd uses those. |
| `!` entries have no effect | See the section above. |
| A description is not used | See the section above. |

Naemon warns about a duplicate service, a host without contacts, a contact
without a notification period and a deprecated main setting. checkd warns
about those too.

Naemon warns when `illegal_macro_output_chars` is not set. checkd does not.

## Where checkd refuses what Naemon accepts

Each row has a corpus case that shows Naemon 1.5.2 accepting the input.
checkd gives an error instead.

| Input | Naemon 1.5.2 | checkd's reason | Case |
| --- | --- | --- | --- |
| `check_interval 0x10` | Stores 16 | A whole-number setting reads `0x10` as 0, so the same text means two things. | `bad-number-hex` |
| `check_interval inf` | Stores `inf` | An interval without end cannot be scheduled. | `bad-number-inf` |
| `check_interval nan` | Stores `nan` | Not a number. | `bad-number-nan` |
| `check_interval 1e400` | Stores `inf` | The same as `inf`. | `bad-number-too-large-for-interval` |
| `max_check_attempts 99999999999` | Stores 1215752191 | The stored number is not the one written. | `bad-number-too-large` |
| `first_notification -1` | Stores -1 | Notifications are counted from 1. What -1 does later in Naemon was not looked at. | `bad-escalation-negative-notification` |
| `hostextinfo`, `serviceextinfo` | Applies them, with a "deprecated" warning | Not written yet. See "Not done yet". | `bad-extinfo` |

Each of these is a choice that can be reversed. A config that relies on
one of them loads in Naemon and not in checkd.

## Other differences, not measured

No corpus case shows what Naemon does in these three places, so each is a
statement about checkd only.

- **`cfg_dir`.** The files of a directory are read in sorted order, so the
  result does not depend on the file system.
- **Bytes that are not UTF-8** are replaced, with a warning.
- **Regular expressions** use the Rust `regex` crate, not the C library.
  The two agree on the simple patterns in the corpus. They differ in less
  common syntax, which has not been mapped.

## Not done yet

- `include_file` and `include_dir` in the main config are an error: "not
  supported yet".
- `broker_module` is ignored with a warning. checkd will not load event
  broker modules.
- `hostextinfo` and `serviceextinfo` definitions are an error: "unknown
  object type". Naemon 1.5.2 still applies them.
- Main config settings are read and checked for form. Nothing uses most
  of them yet.
- Timeperiods are stored as written. Nothing works out yet whether a given
  moment is inside one.
- Loading has not been timed, and no large config has been tried.
