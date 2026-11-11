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
  and refuse it when Naemon exits 1. Fourteen cases are listed in the test as
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

A second test checks 52 warnings, each at its line. Every kind of warning
in the table further down has at least one.

`real/run` in the repo's top directory does the same comparison for config
trees that other people published, for the config the Naemon package
installs and for the config of an OMD demo site. See
[`../../real`](../../real).

## Where checkd copies a doubtful Naemon behaviour

In these places Naemon 1.5.2 does something that looks like a fault, or
like a trace of how it works inside. checkd does the same, so that the same
objects come out, and warns. The corpus README has each rule in full, with
the cases that show it.

- **`!` in a host escalation, host dependency or `hostextinfo`.** A `!`
  takes hosts out of the named host groups only. A host that `host_name` or
  `dependent_host_name` names stays, so `host_name alpha,beta,!beta` still
  includes beta.
- **A host escalation for a host that comes up twice.** The escalation is
  made once each time: a host in two of the named groups gets it twice.
- **A host dependency with an undefined name.** Naemon prints an error,
  drops the dependency and accepts the configuration. The same fault in
  any other object refuses it. This holds while
  `allow_empty_hostgroup_assignment` is on or not given. With the setting
  off, Naemon refuses such a host dependency, and one with a single side
  or a side that comes to no host, and so does checkd. The config that the
  Naemon package installs sets it off.
- **A main setting that is not written plainly.** The three
  `..._skip_check_..._status` settings are read by their leading digits,
  and `allow_empty_hostgroup_assignment` by whether it starts with a
  number above 0.
- **A host pattern in the members of a service group.** With regular
  expressions on, a pair whose host half is a pattern takes every service
  on the matching hosts. The description half is not used.
- **A setting given twice.** For most settings the later line counts. For
  group members and most option lists the lines are added together.
- **`register 0` on an extinfo definition** does not switch it off.
- **A definition with a template `name` and no object name** is taken for a
  template, though it does not say `register 0`.

Each one is a choice that can be reversed: the more useful behaviour is
often plain, at the price of objects that differ from Naemon's for the same
config. Refusing the config is the other way out. That was done for a
service defined twice, where Naemon's result is the hardest to predict
from the file: see "Where checkd refuses what Naemon accepts".

## Warnings that Naemon does not give

Naemon accepts each of these without a word. checkd accepts them too and
warns.

| Warning | When |
| --- | --- |
| A setting is given twice | The same setting, or a day of a timeperiod, twice in one definition. The warning says whether the later line is used or the lines are added together. |
| A setting has no value | A setting name with nothing after it. It is kept as an empty value. |
| A setting has no effect | `failure_prediction_enabled` or `parallelize_check`, from older Nagios versions. |
| Text after the opening brace is not read | `define host { host_name x` on one line. |
| A definition is used as a template only | It has a template `name`, lacks its object name and does not say `register 0`. |
| A template uses itself | Directly or through other templates. The loop adds nothing. |
| A dependency is dropped | It names no failure states, a host dependency lacks a side, or a service dependency names nothing to depend on. |
| A host dependency is dropped | It names an undefined host or host group, and `allow_empty_hostgroup_assignment` is on or not given. Naemon prints an error here and still accepts the configuration. |
| A service dependency matches no pair of services | Its lists are valid and leave nothing. |
| A service applies to no host | Its host group is empty, or `!` removed every host. |
| An extinfo has no effect | It names no host, or its lists leave none. |
| A member of a service group does nothing | `host,!description`. |
| An escalation has no range | Neither `first_notification` nor `last_notification`. |
| An escalation covers no notification | `first_notification` is after `last_notification`. |
| A host has no `check_command` | It is never checked. |
| No hosts are defined | |
| No object files are named | The main config has no `cfg_file` or `cfg_dir`. |
| A value is not a plain number | Something follows the digits, or there are no digits: `3abc`, `3 4`, `often`. The warning names the number Naemon reads, and checkd uses that number. |
| A main setting is not a plain number | A `..._skip_check_..._status` setting such as `3x`, or `allow_empty_hostgroup_assignment` written as anything but `0` or `1`. The warning names what Naemon reads, and checkd uses that. |
| A time is not written HH:MM-HH:MM | Minutes above 59, a single digit for the minutes, seconds, extra zeros. The warning names the times Naemon reads, and checkd uses those. |
| `!` entries have no effect | See the section above. |
| A named host stays | See the section above. |
| A host escalation is made more than once | See the section above. |
| A description is not used | See the section above. |
| `register 0` does not switch an extinfo off | See the section above. |

Naemon warns about a host without contacts, a contact without a
notification period, a deprecated main setting and an extinfo definition.
checkd warns about those too. Naemon also warns about a service defined
twice in the same way, which checkd refuses. For a service defined once by
host name and once by host group Naemon is silent, and so is checkd: that
is the usual way to give one host of a group its own values.

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
| A service defined twice for one host, both times by host name or both times by host group | Warns and keeps one | Which one Naemon keeps differs from host to host, by where the host stands in each definition's lists. Nobody can tell from the file which values a host gets. | `bad-duplicate-service` and the four cases beside it |
| An `include_file` that is not there, named in an included file or in the main file | Prints "Error" and goes on | The settings in that file are missing from the result, and exit 0 says the config is fine. | `bad-include-missing-file` |
| A setting Naemon does not know, or a bad value, in an included file | Prints "Error", skips the rest of that file and goes on | The same: the lines after the fault are dropped without a fault being counted. | `bad-include-unknown-setting` |
| Included files that include each other | Prints "Error" and goes on | A loop is a mistake in the config. | `bad-include-loop` |

Each of these is a choice that can be reversed. A config that relies on
one of them loads in Naemon and not in checkd.

The three rows about included files come from one Naemon rule: a fault in
an included file is printed and is not a fault of the config. The same
fault in the main file refuses it. checkd treats both files alike. An
object definition in an included file is an error in checkd too. What
Naemon does with one was looked at with configs written for the purpose
and has no corpus case.

The row about a service defined twice is the one most likely to be met. One of the twelve published
trees defines a service in two files for the same five hosts, so checkd
refuses that tree. The error names the service, up to three of the hosts
and both places. The definition by host name that overrides one by host
group is not touched by this: it loads as in Naemon.

Until this was changed, checkd copied Naemon's choice host by host, and
gave the same objects as Naemon for every duplicate in the corpus and for
that tree. The code for that is in the history of this repo, in the commit
before the change, if a setting to load such configs the Naemon way is
wanted later.

## Other differences, not measured

No corpus case shows what Naemon does in these places, so each is a
statement about checkd only.

- **`cfg_dir`.** The files of a directory are read in sorted order, so the
  result does not depend on the file system. The order matters where two
  extinfo definitions in different files set the same value. The ten
  published trees that both accept came out the same as Naemon's. That
  shows the order did not change the result there, not that it never does.
- **`include_dir`.** The same: checkd reads the files sorted by name, and
  Naemon in the order the file system gives them. The order matters where
  two files of one directory set the same setting.
- **A `config_rel_path` that is not an absolute path** is an error. Naemon
  takes such a path from the directory it was started in, so the same
  config means different things from different directories.
- **An `include_file` Naemon refuses and checkd reads.** Naemon 1.5.2
  answers "explicitly included more than once" to an `include_file` with
  an absolute path that comes straight after an include which itself
  included a file, though no file is included twice. The message goes away
  with a relative path, or with another include line in between. This is
  what was seen from outside, on 37 configs written to pin it
  down. Why it happens was not looked at. checkd has no such rule.
- **`allow_empty_hostgroup_assignment` with a number too large to hold**
  is read as on. What Naemon makes of it was not tried.
- **Bytes that are not UTF-8** are replaced, with a warning.
- **Regular expressions** use the Rust `regex` crate, not the C library.
  The two agree on the simple patterns in the corpus. They differ in less
  common syntax, which has not been mapped.

## Not done yet

- `broker_module` is ignored with a warning. checkd will not load event
  broker modules.
- Main config settings are read and checked for form. Nothing uses most
  of them yet. The settings known are the ones on the docs page and
  nine more that the Naemon package's config and an OMD site's config
  use. A config with another setting Naemon knows is refused as having an
  unknown setting.
- Timeperiods are stored as written. Nothing works out yet whether a given
  moment is inside one.
- Loading has not been timed, and no large config has been tried.
