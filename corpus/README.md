# Config corpus

Configs written for this project, each with what the reference Naemon makes
of it. The config loader is tested against these: it has to accept what
Naemon accepts, refuse what Naemon refuses, and end up with the same objects.
Seven cases are the exception: Naemon accepts them and checkd refuses them
on purpose. They are listed under "Fault cases".

Nothing here is copied from Naemon's or Nagios's sample configs. The cases
were written from the public docs, and where the docs were silent the
oracle was asked. See [`../CONTRIBUTING.md`](../CONTRIBUTING.md).

## Layout

    corpus/<case>/config/      the input: naemon.cfg and what it refers to
    corpus/<case>/expected/    the oracle's answer, written by oracle/regen

`expected/` holds `verify.txt`, `verify.exit`, `objects.precache` (only when
Naemon wrote one) and `oracle.version`. [`../oracle/README.md`](../oracle/README.md)
says what each file is. Do not edit `expected/` by hand.

Most cases share one `common.cfg`: two commands, a timeperiod, a contact and
a template each for hosts, services and contacts. It is copied into each
case so that a case can be read and run alone.

A case named `bad-<something>` has input that is wrong or doubtful on
purpose. The name says nothing about what Naemon does with it: some of them
Naemon accepts. `expected/verify.exit` has the answer.

To add a case, make `corpus/<name>/config/naemon.cfg`, run
`oracle/regen <name>`, read the result, and commit both directories.

## Cases that are meant to load

| Case | What it covers |
| --- | --- |
| `minimal` | One host and one service, with everything they need |
| `one-of-each` | One object of every type, to show the order of the dump |
| `all-settings` | Every documented host, service and contact setting, option letters, number formats |
| `syntax` | Spacing, comments, continuation lines, older setting names, a file with no final newline |
| `crlf-line-endings` | An object file with Windows line endings |
| `non-ascii` | UTF-8 text in values and names |
| `names` | Letter case, spaces and punctuation in names, and how names sort |
| `cfg-dir` | `cfg_dir` with subdirectories, files it skips, relative and absolute `cfg_file` |
| `definition-order` | Everything used before it is defined |
| `main-settings` | Every setting on the main configuration page of the docs, and a resource file |
| `main-check-timeouts` | `host_check_timeout` and `service_check_timeout` as the default for hosts and services |
| `templates-chain` | Templates through several levels, for hosts, services, contacts, timeperiods and commands |
| `templates-multiple` | Several templates in one `use`, and which one wins |
| `unregistered` | `register 0` on objects that are not templates |
| `inherit-null-additive` | `null` to cancel an inherited value, `+` to add to one |
| `setting-twice` | The same setting twice in one definition |
| `display-name` | `display_name` the same as the name, different, and not set |
| `loose-numbers` | Numbers with letters, spaces, commas or a fraction after the digits, or no digits at all |
| `options-every-letter` | Option lists that name every letter one by one |
| `contact-defaults` | A contact with only the settings it has to have |
| `custom-vars` | Custom variables: inheritance, letter case, `null`, `+`, duplicates |
| `implied-inheritance` | What services take from their host, and escalations from their host or service |
| `check-command-override` | `!` in front of a template's `check_command` |
| `hostgroups` | Members listed in the group, on the host, through other groups, and `*` |
| `hostgroup-exclusion` | `!` in the members of a host group |
| `servicegroups` | The same for service groups |
| `servicegroup-wildcard` | `*` as the description in the members of a service group |
| `contactgroups` | The same for contact groups, and every contact setting |
| `service-expansion` | One service on many hosts: lists, groups, `*`, `!` |
| `service-duplicates` | The same service defined for a group and for one host of the group |
| `parents` | Parent hosts and parent services |
| `service-parents` | Parent services written as pairs of host and description |
| `dependencies` | Host and service dependencies with lists, groups and `*` |
| `dependency-exclusions` | `!` in each list of a host or service dependency |
| `escalations` | Host and service escalations with lists, groups and `*` |
| `escalation-exclusions` | `!` in each list of a host or service escalation |
| `timeperiods` | Weekdays, several ranges, every documented exception format, `exclude` |
| `timeperiod-short-spans` | Spans whose second end leaves out what it shares with the first |
| `loose-times` | Times of day with minutes above 59, single digits, seconds or extra zeros |
| `regexp` | `use_regexp_matching=1` |
| `regexp-servicegroups` | Patterns in the members of a service group |
| `true-regexp` | `use_true_regexp_matching=1` |

## Fault cases

Naemon 1.5.2 refuses a config at one of two points. While reading, it stops
at the first fault it meets and reports only that one. In the pre-flight
check that follows, it goes on and counts every fault.

Refused while reading (exit 1, one fault reported):

`bad-command-no-line`, `bad-define-in-capitals`, `bad-duplicate-host`,
`bad-duplicate-template-name`, `bad-exclusion-of-unknown-host`,
`bad-hostgroup-loop`, `bad-hostgroup-members-exclusion`,
`bad-hostgroup-unknown-member`, `bad-host-name-only`,
`bad-host-no-max-check-attempts`, `bad-host-no-name`,
`bad-illegal-name-char`, `bad-main-empty-value`, `bad-main-no-equals-sign`,
`bad-main-unknown-setting`, `bad-missing-cfg-dir`, `bad-missing-cfg-file`,
`bad-negative-interval`, `bad-number-no-digits`,
`bad-old-name-checks-enabled`,
`bad-option-letter-in-capitals`, `bad-option-letter-wrong-type`,
`bad-parent-is-self`, `bad-regexp-servicegroup-star-host`,
`bad-servicegroup-odd-members`, `bad-servicegroup-star-host`,
`bad-servicegroup-unknown-member`, `bad-service-name-only`,
`bad-service-no-check-command`, `bad-service-no-description`,
`bad-service-no-host`, `bad-service-parents-odd`,
`bad-service-parents-two-descriptions`, `bad-service-unknown-host`,
`bad-setting-in-capitals`, `bad-setting-outside-definition`,
`bad-stray-closing-brace`, `bad-text-for-number`,
`bad-timeperiod-excludes-unknown`, `bad-timeperiod-hour-25`,
`bad-timeperiod-letters-for-minutes`,
`bad-timeperiod-minutes-past-midnight`, `bad-timeperiod-no-alias`,
`bad-timeperiod-no-colon`, `bad-timeperiod-short-span-with-month`,
`bad-timeperiod-space-in-ranges`,
`bad-two-faults`, `bad-two-for-flag`, `bad-unclosed-at-end-of-file`,
`bad-unclosed-definition`, `bad-unknown-check-command`,
`bad-unknown-contact`, `bad-unknown-contactgroup`,
`bad-unknown-event-handler`, `bad-unknown-hostgroup`,
`bad-unknown-object-type`, `bad-unknown-parent`,
`bad-unknown-servicegroup`, `bad-unknown-setting`, `bad-unknown-template`,
`bad-unknown-timeperiod`, `bad-zero-check-attempts`

Refused in the pre-flight check (exit 1, every fault counted):

`bad-contact-name-only`, `bad-dependency-on-itself`,
`bad-host-dependency-loop`, `bad-parent-loop`,
`bad-service-dependency-loop`, `bad-timeperiod-excludes-itself`

Accepted with a warning (exit 0):

| Case | What Naemon did |
| --- | --- |
| `bad-duplicate-service` | Kept the definition that comes later in the file |
| `bad-host-no-contacts` | Loaded the host with no contacts |

Accepted without a word (exit 0):

| Case | What Naemon did |
| --- | --- |
| `bad-template-loop`, `bad-template-uses-itself` | Loaded the host; the loop adds nothing |
| `bad-dependency-no-criteria` | Dropped the dependency: it is not in the dump |
| `bad-service-on-empty-hostgroup`, `bad-exclusion-leaves-nothing` | Created no service |
| `bad-escalation-no-range` | Wrote first and last notification as -2 |
| `bad-escalation-first-after-last` | Kept first 5, last 2 |
| `bad-setting-without-value` | Kept the setting with an empty value |
| `bad-timeperiod-backwards-range` | Kept `17:00-09:00` |
| `bad-timeperiod-day-twice` | Kept the later line for that day |
| `bad-timeperiod-impossible-date` | Kept `2027-02-30` and `february 31` |
| `bad-name-with-angle-bracket` | Loaded the host; `illegal_object_name_chars` has no default |
| `bad-host-no-check-command` | Loaded the host with no check command |
| `bad-main-no-object-files`, `bad-no-hosts` | Loaded a config with no hosts |

Accepted by Naemon (exit 0), refused by checkd on purpose:

| Case | Input | What Naemon did |
| --- | --- | --- |
| `bad-number-hex` | `check_interval 0x10` | Stored 16 |
| `bad-number-inf` | `check_interval inf` | Stored `inf` |
| `bad-number-nan` | `check_interval nan` | Stored `nan` |
| `bad-number-too-large-for-interval` | `check_interval 1e400` | Stored `inf` |
| `bad-number-too-large` | `max_check_attempts 99999999999` | Stored 1215752191 |
| `bad-escalation-negative-notification` | `first_notification -1` | Stored -1 |
| `bad-extinfo` | `hostextinfo` and `serviceextinfo` definitions | Put their `notes` on the host and the service, with a "deprecated" warning |

[`../crates/checkd-config/README.md`](../crates/checkd-config/README.md)
says why checkd refuses each one.

## What the oracle showed

Each point below was seen in the case named, with Naemon Core 1.5.2. A point
is what one case showed, not a rule checked from several sides.

### Files and syntax

- `;` starts a comment anywhere in a line, with or without a space before
  it. `\;` gives a literal semicolon. `#` starts a comment only at the start
  of a line: after a value it is part of the value. (`syntax`)
- A `\` at the end of a line joins the next line to it. The spaces at the
  start of the next line are dropped; the space before the `\` is kept.
  (`syntax`)
- Spaces inside a value are kept as written. Spaces and tabs at the end of a
  line are dropped. (`syntax`)
- `define` and setting names must be in lower case. (`bad-define-in-capitals`,
  `bad-setting-in-capitals`)
- `normal_check_interval` and `retry_check_interval` are accepted as older
  names. `checks_enabled` is not. (`syntax`, `bad-old-name-checks-enabled`)
- A setting given twice in one definition: the later line counts, with no
  warning. That holds when one line uses the older name of the setting.
  (`setting-twice`)
- CR LF line endings load. A last line with no newline loads.
  (`crlf-line-endings`, `syntax`)
- Non-ASCII bytes pass through unchanged, in names too. (`non-ascii`)
- Objects can be used before they are defined, across files.
  (`definition-order`)
- `cfg_dir` goes into subdirectories and reads files ending in `.cfg`. It
  skips `.CFG`, other endings, and files and directories whose name starts
  with a dot. (`cfg-dir`)
- A relative `cfg_file` or `resource_file` is found from the main config's
  directory. (`cfg-dir`, `main-settings`)

### Main config

- All but one of the settings on the docs page are accepted. Fourteen are
  accepted with a "deprecated" warning: `naemon_user`, `naemon_group`,
  `service_inter_check_delay_method`, `max_service_check_spread`,
  `service_interleave_factor`, `host_inter_check_delay_method`,
  `max_host_check_spread`, `auto_reschedule_checks`,
  `auto_rescheduling_interval`, `auto_rescheduling_window`,
  `use_large_installation_tweaks`, `free_child_process_memory`,
  `child_processes_fork_twice` and `enable_environment_macros`.
  (`main-settings`)
- These warnings are not counted in "Total Warnings". (`main-settings`)
- A setting with an empty value is an error, so `loadctl_options=` as the
  docs show it does not load. So is a line with no `=`, and an unknown
  setting. (`bad-main-empty-value`, `bad-main-no-equals-sign`,
  `bad-main-unknown-setting`)
- `$USERn$` macros are left as written in the dump. (`main-settings`)
- `host_check_timeout` and `service_check_timeout` are the `check_timeout`
  of a host or service that sets none. Without them it is 30 for a host and
  60 for a service. (`main-check-timeouts`, `minimal`)
- A config that names no object files, or defines no hosts, is accepted
  with no warning about it. (`bad-main-no-object-files`, `bad-no-hosts`)

### Templates

- Lookup goes depth first, left to right: the first template in `use`, then
  that template's own templates, and only then the second template in `use`.
  (`templates-multiple`, hosts gamma and delta)
- Spaces after the commas in `use` are fine. (`templates-multiple`)
- A registered object with a `name` works as a template too, and passes on
  its `alias` and `display_name`. (`templates-chain`)
- `register 0` removes an object whether or not it is a template, and is not
  inherited. (`unregistered`)
- A template loop is not reported. (`bad-template-loop`)
- A `!` in front of a template's `check_command` makes it win over the
  service's own, also when that template is listed second.
  (`check-command-override`)

### `null` and `+`

- `null` removes an inherited value for `event_handler`, `notes`,
  `hostgroups`, `servicegroups`, `parents` and `contacts`.
  (`inherit-null-additive`, `parents`)
- `+` adds to an inherited list for `hostgroups`, `servicegroups`,
  `contacts`, `contact_groups` and `parents`, through more than one level.
  With nothing inherited, `+x` is the same as `x`. (`inherit-null-additive`,
  `parents`)
- On a setting that is not a list, the `+` stays in the value: `notes`
  becomes `+and in zeta`. (`inherit-null-additive`)
- On a service, `contacts +dev` does not pull in the host's contacts. On an
  escalation, it does pull in the host's or service's. (`implied-inheritance`)

### Custom variables

- Names are written in upper case. (`custom-vars`)
- `null` and `+` have no special meaning: the value is the text `null` or
  `+team-b`. (`custom-vars`)
- The same name twice in one object gives two entries. (`custom-vars`)

### What a service takes from its host

- `contacts`, `contact_groups`, `check_period`, `notification_period` and
  `notification_interval` come from the host when the service and its
  templates set none of them. (`implied-inheritance`)
- If the service sets either `contacts` or `contact_groups`, it gets neither
  from the host. That holds when the service sets `contacts null`.
  (`implied-inheritance`, `inherit-null-additive`)
- `notification_interval` comes from the host only when the host sets it. An
  escalation on a host that relies on the default gets 0, not 30.
  (`escalations`, `implied-inheritance`)

### Groups

- A group gets its name as its alias when none is given. A timeperiod does
  not: no alias is an error. (`hostgroups`, `bad-timeperiod-no-alias`)
- `!host` works in host group `members`, which the docs do not mention.
  `!group` in `hostgroup_members` does not: it is read as a group name.
  (`hostgroup-exclusion`, `bad-hostgroup-members-exclusion`)
- `members *` works for contact groups as well as host groups.
  (`contactgroups`)
- Spaces around the commas in `members` are fine. A host listed twice is a
  member once. An empty group is fine. (`hostgroups`)
- Group membership loops are an error. (`bad-hostgroup-loop`)
- In the members of a service group, `*` can stand for the description
  (`web-1,*` is every service on web-1) and not for the host (`*,Other` is
  an error, with or without regular expressions). (`servicegroup-wildcard`,
  `bad-servicegroup-star-host`, `bad-regexp-servicegroup-star-host`)

### Services on many hosts

- A service that sets both `host_name` and `hostgroup_name` goes on the
  hosts of both. That holds when the group comes from a template.
  (`service-expansion`)
- `!` works whichever side of the list it is on, and `host_name !x` can
  remove a host that `hostgroup_name` brought in. (`service-expansion`)
- When a service is defined for a group and again for one host of the group,
  the host's own definition wins, in either order, with no warning.
  (`service-duplicates`)
- When it is defined twice by host name, the later one wins, with a warning.
  (`bad-duplicate-service`)

### Dependencies and escalations

- Each dependency is split in two in the dump: one entry with
  `execution_failure_options` and one with `notification_failure_options`.
  A side with no criteria gives no entry, so a dependency with no criteria
  at all disappears. (`dependencies`, `bad-dependency-no-criteria`)
- With no `dependent_host_name`, a service dependency stays on the same
  host. (`dependencies`)
- `escalation_options` defaults to `a`. (`escalations`)
- In a host escalation or host dependency, a `!host` item in a host list
  has no effect unless the host group setting beside it is given too:
  `host_name alpha,beta,gamma,!beta` and `host_name *,!beta` both still
  include beta. With `hostgroup_name hg-all` and `host_name !beta`, beta is
  left out. `!group` in the host group list works. This looks like a fault
  in Naemon rather than a rule. (`escalation-exclusions`,
  `dependency-exclusions`, `escalations`)
- In a service escalation or service dependency, `!` works in the host
  list, the host group list and the description list, on both sides of a
  dependency. (`escalation-exclusions`, `dependency-exclusions`)

### Service parents

- One item in `parents` is a description on the service's own host. Two or
  more items are pairs of host name and description, so `parents Ping,Load`
  looks for the service 'Load' on a host 'Ping'. An odd number of three or
  more is an error. (`parents`, `service-parents`,
  `bad-service-parents-two-descriptions`, `bad-service-parents-odd`)
- The dump writes a single parent on the service's own host as the
  description alone, also when it was given as a pair. With two or more
  parents, all are written as pairs. (`service-parents`)

### Timeperiods

- `july 10 - 15` is written back as `july 10 - july 15`. (`timeperiods`)
- No space is allowed after the comma between two ranges.
  (`bad-timeperiod-space-in-ranges`)
- Dates that do not exist and ranges that run backwards are accepted.
  (`bad-timeperiod-impossible-date`, `bad-timeperiod-backwards-range`)
- `monday 1 - 3` is accepted and written back as `monday 1 - monday 3`.
  `tuesday 1 april - 3` is an error. (`timeperiod-short-spans`,
  `bad-timeperiod-short-span-with-month`)
- Minutes above 59 are carried into the hours: `09:75-17:00` is written
  back as `10:15-17:00`, `09:00-17:99` as `09:00-18:39` and `09:00-23:60`
  as `09:00-24:00`. A time that ends up after 24:00 is an error.
  (`loose-times`, `bad-timeperiod-minutes-past-midnight`,
  `bad-timeperiod-hour-25`)
- `9:5` is read as `09:05` and `009:00` as `09:00`. Seconds are dropped:
  `09:00:30-17:00` is written back as `09:00-17:00`. (`loose-times`)
- A time with no colon, or with letters where the minutes belong, is an
  error. (`bad-timeperiod-no-colon`, `bad-timeperiod-letters-for-minutes`)

### Regular expressions

- With `use_regexp_matching=1`, `web-*` is a regular expression, not a shell
  wildcard: it means "web" and any number of "-", so it also matches
  `webcam`. (`regexp`)
- Patterns are not anchored: `eb-1.*` matches `web-1` and `web-10`.
  (`regexp`)
- A dotted host name used as a plain name still finds its host. (`regexp`)
- Patterns work in `host_name`, `hostgroup_name`, group `members` and an
  escalation's `service_description`. (`regexp`)
- In the members of a service group, when the host half of a pair is a
  pattern, every service on the matching hosts becomes a member and the
  description half is not looked at: `web-.*,Star suffix`, `web-.*,^Other$`
  and `web-.*,Nothing like it` all give every service on web-1 and web-2.
  When the host is a plain name, a pattern in the description works. This
  looks like a fault in Naemon rather than a rule. A service escalation or
  dependency with a host pattern does use the description.
  (`regexp-servicegroups`)

### Values

- Option letters are lower case only. (`bad-option-letter-in-capitals`)
- Numbers: `.5`, `2.`, `+7`, `1e1` and `007` are all accepted. Intervals are
  written with six decimals, so `3.14159265358979` becomes `3.141593`.
  (`all-settings`)
- A number is read from its leading digits and the rest of the value is
  dropped. For a whole number, `3abc`, `3 4`, `3,4` and `3.7` are 3, `1e2`
  is 1 and `5 # five tries` is 5. For an interval, `5x`, `5,5` and `5 6`
  are 5, `1e1x` is 10, `7e` is 7 and `.5.5` is 0.5. None of these gives a
  warning. (`loose-numbers`)
- A value with no leading digits is read as 0: `freshness_threshold x`,
  `check_interval often`, `first_notification two`. For
  `max_check_attempts` that is an error, because 0 is not allowed there.
  (`loose-numbers`, `bad-number-no-digits`, `bad-text-for-number`)
- `check_interval -0` is written as `-0.000000`. A negative interval is an
  error. A negative `first_notification` is not: -1 is stored.
  (`loose-numbers`, `bad-negative-interval`,
  `bad-escalation-negative-notification`)
- An interval also takes `0x10` (16), `inf` and `nan`, and `1e400` becomes
  `inf`. A whole number too large for 32 bits wraps: `99999999999` is
  stored as 1215752191. (`bad-number-hex`, `bad-number-inf`,
  `bad-number-nan`, `bad-number-too-large-for-interval`,
  `bad-number-too-large`)
- `hostextinfo` and `serviceextinfo` definitions are accepted with the line
  "WARNING: Extinfo objects are deprecated and will be removed in future
  versions". Their `notes` end up on the host and the service.
  (`bad-extinfo`)
- `max_check_attempts` has to be given and be 1 or more. A check command is
  required for a service and not for a host. (`bad-host-no-max-check-attempts`,
  `bad-service-no-check-command`, `bad-host-no-check-command`)
- A flag takes 0 or 1 only. (`bad-two-for-flag`)
- A contact that sets no notification options gets `n` for both. A contact
  with no notification periods loads, with a warning for each.
  (`contact-defaults`)
- `display_name` is written when set, also when it is the same as the name.
  (`display-name`)
- An option list that names every letter is written as the letters, not as
  `a`. (`options-every-letter`)

### The dump itself

- Object types come in this order: timeperiod, command, contactgroup,
  hostgroup, servicegroup, contact, host, service, servicedependency,
  serviceescalation, hostdependency, hostescalation. (`one-of-each`)
- Within a type, objects are sorted by name, byte by byte, so capitals come
  before lower case. (`names`)
- Services are not sorted by host and then description. The order seen fits
  sorting on `host;description` as one string, which puts `web-10` before
  `web-1`. (`names`)
- Host group members are sorted. Service group and contact group members,
  dependencies and escalations come in an order that was not worked out.
  (`hostgroups`, `servicegroups`, `contactgroups`, `dependencies`)
- `contacts`, `contact_groups`, notification commands and `exclude` are
  written in the reverse of the order given. `parents` is not.
  (`contactgroups`, `all-settings`, `timeperiods`, `parents`)
- Option lists are written in a fixed order, whatever order was given.
  (`all-settings`)
- Timeperiod exceptions are grouped by kind, and ranges within a day are
  sorted by start time. (`timeperiods`)
- Custom variables come last in an object: the object's own in the order
  written, then the inherited ones in the reverse of the template's order.
  (`custom-vars`)
- `initial_state` is always written as `o`, whatever was set. The dump
  cannot show whether `initial_state` was taken in. (`all-settings`)
- `hostgroups` and `servicegroups` are not written on the host or service.
  Membership shows only in the group. (`inherit-null-additive`)

## Not covered yet

- Any setting or object type that is not on the docs pages used:
  object definitions, object inheritance, time-saving tricks, main
  configuration.
- `include_file`, `include_dir` and `broker_module` in the main config.
- Numbers in settings other than the ones tried: `max_check_attempts`,
  `check_interval`, `freshness_threshold` and the three numbers of a host
  escalation.
- `hostextinfo` and `serviceextinfo` settings other than `notes`.
- Files that are not UTF-8.
- Regular expressions beyond the simple forms in the regexp cases.
- Very large configs. That belongs to the load benchmark.
- What Naemon does after loading. `initial_state` is one example.
