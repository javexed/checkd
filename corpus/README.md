# Config corpus

Configs written for this project, each with what the reference Naemon makes
of it. The config loader is tested against these: it has to accept what
Naemon accepts, refuse what Naemon refuses, and end up with the same objects.
Six cases are the exception: Naemon accepts them and checkd refuses them
on purpose. They are listed under "Fault cases".

Nothing here is copied from Naemon's or Nagios's sample configs. The cases
were written from the public docs, and where the docs were silent the
oracle was asked. Some cases rebuild, with invented names, something first
seen in a config tree that other people published:
[`../real/README.md`](../real/README.md) says how those trees are used. See
[`../CONTRIBUTING.md`](../CONTRIBUTING.md) for the rule.

## Layout

    corpus/<case>/config/      the input: naemon.cfg and what it refers to
    corpus/<case>/expected/    the oracle's answer, written by oracle/regen

`expected/` holds `verify.txt`, `verify.exit`, `objects.precache` (only when
Naemon wrote one) and `oracle.version`. [`../oracle/README.md`](../oracle/README.md)
says what each file is. Do not edit `expected/` by hand.

Most cases share one `common.cfg`: two commands, a timeperiod, a contact and
a template each for hosts, services and contacts. It is copied into each
case so that a case can be read and run alone. The cases on lists of names
share a second file, `base.cfg`, with three hosts, three groups of each
kind and two services, and keep only what they test in `objects.cfg`.

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
| `define-line-forms` | Ways to write the first and last line of a definition |
| `short-names` | Older and shorter names of settings that are still read |
| `settings-without-effect` | `failure_prediction_enabled` and `parallelize_check`: accepted and not used |
| `crlf-line-endings` | An object file with Windows line endings |
| `non-ascii` | UTF-8 text in values and names |
| `names` | Letter case, spaces and punctuation in names, and how names sort |
| `cfg-dir` | `cfg_dir` with subdirectories, files it skips, relative and absolute `cfg_file` |
| `definition-order` | Everything used before it is defined |
| `main-settings` | Every setting on the main configuration page of the docs, and a resource file |
| `main-check-timeouts` | `host_check_timeout` and `service_check_timeout` as the default for hosts and services |
| `templates-chain` | Templates through several levels, for hosts, services, contacts, timeperiods and commands |
| `templates-multiple` | Several templates in one `use`, and which one wins |
| `templates-nameless` | A template `name`, no object name and no `register 0`: taken for a template |
| `unregistered` | `register 0` on objects that are not templates |
| `inherit-null-additive` | `null` to cancel an inherited value, `+` to add to one |
| `null-names` | Objects called `null`, and `null` in lists of names |
| `null-texts` | `null` in free-text settings |
| `setting-twice` | The same setting twice in one definition |
| `setting-twice-members` | A list setting twice: group members add up, other lists do not |
| `setting-twice-options` | An option list twice: most add up, and `n` clears |
| `setting-twice-templates` | Settings given twice, together with templates, `+` and `null` |
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
| `own-groups-exclusions` | `!` in an object's own `hostgroups`, `servicegroups` or `contactgroups` |
| `lists-star` | `*` alone in every list that takes it, with regular expressions off |
| `lists-leave-nothing` | Valid lists that leave no host, service or contact, and are accepted |
| `service-expansion` | One service on many hosts: lists, groups, `*`, `!` |
| `service-duplicates` | The same service defined for a group and for one host of the group |
| `duplicate-service-first-host` | A service defined twice by host name: which definition is kept on which host |
| `duplicate-service-hostgroups` | The same, both times by host group |
| `duplicate-service-patterns` | The same, both times by pattern |
| `duplicate-service-mixed` | Definitions that do not name the same hosts, and by name against by group |
| `parents` | Parent hosts and parent services |
| `service-parents` | Parent services written as pairs of host and description |
| `dependencies` | Host and service dependencies with lists, groups and `*` |
| `dependency-exclusions` | `!` in each list of a host or service dependency |
| `servicedependency-sides` | The ways each side of a service dependency names its services |
| `hostdependency-one-sided` | Host dependencies that lack a side: accepted, nothing made |
| `hostdependency-dropped` | Host dependencies with an undefined name: dropped, config accepted |
| `escalations` | Host and service escalations with lists, groups and `*` |
| `escalation-exclusions` | `!` in each list of a host or service escalation |
| `serviceescalation-sides` | The ways a service escalation names its services |
| `servicegroup-exclusions` | What `!` takes out of a dependency or escalation when a service group is named |
| `hostescalation-repeated-hosts` | A host escalation is made once each time its lists bring a host up |
| `named-hosts-stay` | `!` does not take out a host that `host_name` names, except for services |
| `timeperiods` | Weekdays, several ranges, every documented exception format, `exclude` |
| `timeperiod-short-spans` | Spans whose second end leaves out what it shares with the first |
| `timeperiod-inherited-exceptions` | The order of exceptions that come from templates |
| `loose-times` | Times of day with minutes above 59, single digits, seconds or extra zeros |
| `regexp` | `use_regexp_matching=1` |
| `regexp-servicegroups` | Patterns in the members of a service group |
| `true-regexp` | `use_true_regexp_matching=1` |
| `regexp-own-groups` | Patterns in an object's own `hostgroups`, `servicegroups` or `contactgroups` |
| `regexp-star-description` | `*` alone as a service description with regular expressions on |
| `regexp-hostdependency-dropped` | `*` with regular expressions on, in a host dependency: dropped |
| `extinfo` | `hostextinfo` and `serviceextinfo` definitions |
| `extinfo-settings` | Every setting the two take |
| `extinfo-targets` | The ways an extinfo names its hosts and services |
| `extinfo-precedence` | Own value, template value and several extinfo values: which one wins |
| `extinfo-templates` | Extinfo templates, and `register 0` on an extinfo |
| `extinfo-regexp` | Patterns in extinfo host names and descriptions |

## Fault cases

Naemon 1.5.2 refuses a config at one of two points. While reading, it stops
at the first fault it meets and reports only that one. In the pre-flight
check that follows, it goes on and counts every fault.

Refused while reading (exit 1, one fault reported), 168 cases.

The main config (5):

`bad-main-empty-value`, `bad-main-no-equals-sign`,
`bad-main-unknown-setting`, `bad-missing-cfg-dir`, `bad-missing-cfg-file`

How definitions and setting names are written (16):

`bad-define-in-capitals`, `bad-old-name-checks-enabled`,
`bad-parallelize-check-on-host`, `bad-setting-in-capitals`,
`bad-setting-outside-definition`, `bad-setting-twice-plus`,
`bad-short-name-hostgroup-on-host`, `bad-short-name-hosts-on-dependency`,
`bad-short-name-hosts-on-escalation`, `bad-stray-closing-brace`,
`bad-text-after-closing-brace`, `bad-two-faults`,
`bad-unclosed-at-end-of-file`, `bad-unclosed-definition`,
`bad-unknown-object-type`, `bad-unknown-setting`

Values (8):

`bad-illegal-name-char`, `bad-negative-interval`, `bad-number-no-digits`,
`bad-option-letter-in-capitals`, `bad-option-letter-wrong-type`,
`bad-text-for-number`, `bad-two-for-flag`, `bad-zero-check-attempts`

Extended info (15):

`bad-extinfo-coords`, `bad-extinfo-custom-variable`,
`bad-extinfo-empty-hostgroup`, `bad-extinfo-excluded-away`,
`bad-extinfo-exclusion-only`, `bad-extinfo-host-setting`,
`bad-extinfo-no-hosts`, `bad-extinfo-old-host-names`,
`bad-extinfo-service-not-on-every-host`, `bad-extinfo-service-setting`,
`bad-extinfo-template-missing`, `bad-extinfo-template-twice`,
`bad-extinfo-unknown-host`, `bad-extinfo-unknown-hostgroup`,
`bad-extinfo-unknown-service`

`*` with regular expressions on (14):

`bad-regexp-servicegroup-star-host`, `bad-regexp-star-after-name`,
`bad-regexp-star-contactgroup-members`, `bad-regexp-star-host`,
`bad-regexp-star-hostescalation`, `bad-regexp-star-hostextinfo`,
`bad-regexp-star-hostgroup`, `bad-regexp-star-hostgroup-members`,
`bad-regexp-star-own-contactgroups`, `bad-regexp-star-own-hostgroups`,
`bad-regexp-star-own-servicegroups`, `bad-regexp-star-servicedependency`,
`bad-regexp-star-serviceescalation-servicegroup`,
`bad-true-regexp-star-description`

An object's own groups (8):

`bad-own-contactgroups-excluded-away`,
`bad-own-contactgroups-exclusion-only`, `bad-own-contactgroups-unknown`,
`bad-own-hostgroups-excluded-away`, `bad-own-hostgroups-exclusion-only`,
`bad-own-hostgroups-pattern-without-regexp`,
`bad-own-servicegroups-excluded-away`, `bad-own-servicegroups-exclusion-only`

Lists that take names only (19):

`bad-contact-groups-exclusion`, `bad-contact-groups-pattern`,
`bad-contact-groups-star`, `bad-contacts-exclusion`, `bad-contacts-pattern`,
`bad-contacts-star`, `bad-hostgroup-members-exclusion`,
`bad-member-contactgroups-exclusion`, `bad-member-contactgroups-pattern`,
`bad-member-contactgroups-star`, `bad-member-hostgroups-exclusion`,
`bad-member-hostgroups-pattern`, `bad-member-hostgroups-star`,
`bad-member-servicegroups-exclusion`, `bad-member-servicegroups-pattern`,
`bad-member-servicegroups-star`, `bad-parents-exclusion`,
`bad-parents-pattern`, `bad-parents-star`

Host escalations (8):

`bad-hostescalation-empty-hostgroup`, `bad-hostescalation-excluded-away`,
`bad-hostescalation-exclusion-only`,
`bad-hostescalation-host-excluded-from-group`,
`bad-hostescalation-host-null`,
`bad-hostescalation-hostgroup-exclusion-only`, `bad-hostescalation-no-host`,
`bad-hostescalation-template-no-host`

Service escalations (15):

`bad-serviceescalation-description-excluded-away`,
`bad-serviceescalation-description-exclusion-only`,
`bad-serviceescalation-description-only`,
`bad-serviceescalation-empty-hostgroup`,
`bad-serviceescalation-empty-servicegroup`,
`bad-serviceescalation-exclusion-only`,
`bad-serviceescalation-group-and-description`,
`bad-serviceescalation-group-and-host`,
`bad-serviceescalation-host-excluded-away`,
`bad-serviceescalation-host-null`,
`bad-serviceescalation-hostgroup-exclusion-only`,
`bad-serviceescalation-no-description`,
`bad-serviceescalation-servicegroup-excluded-away`,
`bad-serviceescalation-servicegroup-exclusion-only`,
`bad-serviceescalation-template-no-service`

Service dependencies (11):

`bad-servicedependency-dependent-description-only`,
`bad-servicedependency-dependent-group-and-description`,
`bad-servicedependency-dependent-host-no-description`,
`bad-servicedependency-descriptions-only`,
`bad-servicedependency-group-and-description`,
`bad-servicedependency-host-no-description`,
`bad-servicedependency-no-dependent`, `bad-servicedependency-no-services`,
`bad-servicedependency-partly-unknown`,
`bad-servicedependency-same-host-no-such-service`,
`bad-servicedependency-template-no-services`

Timeperiods (10):

`bad-timeperiod-excludes-unknown`, `bad-timeperiod-hour-25`,
`bad-timeperiod-letters-for-minutes`, `bad-timeperiod-minutes-past-midnight`,
`bad-timeperiod-no-alias`, `bad-timeperiod-no-colon`,
`bad-timeperiod-no-name`, `bad-timeperiod-short-span-with-month`,
`bad-timeperiod-space-in-ranges`, `bad-timeperiod-template-no-alias`

Names that are not defined (13):

`bad-exclusion-of-unknown-host`, `bad-hostgroup-unknown-member`,
`bad-service-unknown-host`, `bad-servicegroup-unknown-member`,
`bad-unknown-check-command`, `bad-unknown-contact`,
`bad-unknown-contactgroup`, `bad-unknown-event-handler`,
`bad-unknown-hostgroup`, `bad-unknown-parent`, `bad-unknown-servicegroup`,
`bad-unknown-template`, `bad-unknown-timeperiod`

Groups and parents (9):

`bad-hostgroup-loop`, `bad-hostgroup-no-name`, `bad-parent-is-self`,
`bad-service-parents-odd`, `bad-service-parents-two-descriptions`,
`bad-servicegroup-member-excluded-host`, `bad-servicegroup-no-name`,
`bad-servicegroup-odd-members`, `bad-servicegroup-star-host`

Objects that lack a setting they need, or are defined twice (17):

`bad-command-no-line`, `bad-command-no-name`, `bad-command-template-no-line`,
`bad-contact-no-name`, `bad-contactgroup-no-name`, `bad-duplicate-host`,
`bad-duplicate-nameless-template`, `bad-duplicate-template-name`,
`bad-host-name-only`, `bad-host-no-max-check-attempts`, `bad-host-no-name`,
`bad-service-description-null`, `bad-service-host-null`,
`bad-service-name-only`, `bad-service-no-check-command`,
`bad-service-no-description`, `bad-service-no-host`

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
| `bad-service-on-empty-hostgroup`, `bad-exclusion-leaves-nothing` | Created no service. `lists-leave-nothing` has more of the kind |
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

[`../crates/checkd-config/README.md`](../crates/checkd-config/README.md)
says why checkd refuses each one.

## What the oracle showed

Each point below was seen in the case named, with Naemon Core 1.5.2. A point
is what the named cases showed. The rules on lists of names, on settings
given twice and on services defined twice were also tried on a few hundred
throwaway configs that are not kept here, and they hold for those. They are
still rules read off from outside, and may be wrong at an edge nobody tried.

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
- The first line of a definition can be written `define host{`, with
  several spaces or a tab, or indented, and a `;` comment can follow either
  brace. Other text after the opening brace is dropped without a message.
  Text after the closing brace is an error. (`define-line-forms`,
  `bad-text-after-closing-brace`)
- Some older and shorter names of settings are still read:
  `normal_check_interval`, `retry_check_interval`, `obsess_over_host`,
  `obsess_over_service`, `host` for `host_name`, `hostgroup` and
  `hostgroups` for `hostgroup_name`, the same with `dependent_` in front,
  and `execution_failure_criteria` and `notification_failure_criteria`.
  Which object takes which name is not regular. `checks_enabled` is read on
  a host and refused on a service. `hosts` is read on a service and refused
  on a dependency and on an escalation. `hostgroup` is refused on a host.
  (`short-names`, `syntax`, `bad-old-name-checks-enabled`,
  `bad-short-name-hostgroup-on-host`, `bad-short-name-hosts-on-dependency`,
  `bad-short-name-hosts-on-escalation`)
- `failure_prediction_enabled` on a host or service and `parallelize_check`
  on a service are accepted with any value and do nothing.
  `parallelize_check` on a host is an error. (`settings-without-effect`,
  `bad-parallelize-check-on-host`)
- A setting given twice in one definition: for a single value, and for most
  lists of names, the later line counts, with no warning. That holds when
  one line uses the older name of the setting. `use` twice is the same.
  (`setting-twice`, `setting-twice-members`, `setting-twice-templates`)
- The lines of a group's `members` and of its member groups are added
  together, so a `!` or a `*` on one line works on the names of the other.
  They are added as text: a `+` at the start of the second line is read as
  part of a name, and that is an error. (`setting-twice-members`,
  `bad-setting-twice-plus`)
- The lines of most option lists are added together too. An `n` clears
  what came before it, on the same line or an earlier one, and `a` on
  either line gives every option. For `flap_detection_options` the later
  line counts. (`setting-twice-options`)
- Where lines add up and a template sets the same setting, the object's
  sum replaces the template's value. A `+` on the first line makes the sum
  add to the template's value. A line that says `null` adds nothing and
  takes nothing away. A template's two lines are inherited as their sum.
  (`setting-twice-templates`)
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
- A definition with a template `name`, without the setting that names the
  object and without `register 0`, is taken for a template and makes no
  object. Nothing is printed. That holds for a host, a contact, a command,
  a timeperiod, the three kinds of group, a host dependency, and a service
  that lacks its description or its hosts. (`templates-nameless`)
- Only the missing name does that. A command that has its name and a
  template `name` and no `command_line` is an error, and so is a
  timeperiod that has both names and no alias.
  (`bad-command-template-no-line`, `bad-timeperiod-template-no-alias`)
- An escalation or a service dependency that names nobody is an error with
  a template `name` too. (`bad-hostescalation-template-no-host`,
  `bad-serviceescalation-template-no-service`,
  `bad-servicedependency-template-no-services`)
- Two such templates with the same `name` are an error, like any two
  templates. (`bad-duplicate-nameless-template`)
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
- `null` is also a name like any other. There can be a host, a contact, a
  group, a command and a timeperiod called `null`. In a list of names,
  `null` alone means no value, and `null` among other names is the object
  of that name. (`null-names`)
- For `alias` and `address`, `null` is the text "null". For the other
  free-text settings it means no value. (`null-texts`)
- A service whose `host_name` or `service_description` is `null` alone is
  an error, and so is an escalation whose `host_name` is.
  (`bad-service-host-null`, `bad-service-description-null`,
  `bad-hostescalation-host-null`, `bad-serviceescalation-host-null`)

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
- An object's own `hostgroups`, `servicegroups` or `contactgroups` setting
  is a list like the others: it takes `*`, `!group` and, with regular
  expressions on, patterns. The docs do not say so. A list that leaves no
  group is an error, and so is `!group` with nothing before it.
  (`own-groups-exclusions`, `regexp-own-groups`, `lists-star`,
  `bad-own-hostgroups-excluded-away`, `bad-own-hostgroups-exclusion-only`
  and the cases beside them)
- `hostgroup_members`, `servicegroup_members`, `contactgroup_members`,
  `contact_groups`, `contacts` and `parents` take names only. `*`, `!name`
  and a pattern are each looked up as a name, and not found.
  (`bad-member-hostgroups-star` and the 17 cases beside it)
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
- When a service is defined for a group and again by name for one host of
  the group, the definition by name is kept, in either order.
  (`service-duplicates`, `duplicate-service-mixed`)
- When it is defined twice in the same way, by host name both times or by
  host group both times, Naemon keeps one and prints "Warning: Already
  defined in config file". Which one it keeps depends on the "first host"
  of each definition:
  - on the first host of both definitions, the later one is kept;
  - on a host that is the first host of one of them only, the other one is
    kept;
  - on every other host, the earlier one is kept.

  So two definitions for the same single host give the later one, as the
  docs suggest, and two definitions for the same three hosts give the later
  one on one host and the earlier one on the other two.
  (`bad-duplicate-service`, `duplicate-service-first-host`,
  `duplicate-service-mixed`)
- The first host of a definition is the first name written in `host_name`.
  Host groups come before host names, whichever is written first. A group
  gives its hosts in the reverse of the order they were added to it, and
  `*` and a pattern give the last defined host first. A definition has no
  first host when that host comes up again further on in its lists, or is
  excluded. (`duplicate-service-first-host`, `duplicate-service-hostgroups`,
  `duplicate-service-patterns`)
- This looks like the trace of how Naemon stores services while it reads,
  not like a rule anybody chose. checkd copies it and warns.
- A service for no host is accepted and makes nothing: an empty host
  group, or a list that `!` empties. (`bad-service-on-empty-hostgroup`,
  `bad-exclusion-leaves-nothing`, `lists-leave-nothing`)

### Dependencies and escalations

- Each dependency is split in two in the dump: one entry with
  `execution_failure_options` and one with `notification_failure_options`.
  A side with no criteria gives no entry, so a dependency with no criteria
  at all disappears. (`dependencies`, `bad-dependency-no-criteria`)
- With no `dependent_host_name`, a service dependency stays on the same
  host. (`dependencies`)
- `escalation_options` defaults to `a`. (`escalations`)
- In a host escalation, a host dependency and a `hostextinfo`, a `!` takes
  hosts out of what the host groups bring, and nothing else. A host that
  `host_name` names stays, whatever is excluded by name or by group. So
  `host_name alpha,beta,gamma,!beta` and `host_name *,!beta` both still
  include beta, and with `hostgroup_name web` (alpha and beta) and
  `host_name alpha,!alpha` the result is alpha and beta. With
  `hostgroup_name hg-all` and `host_name !beta`, beta is left out. `!group`
  in the host group list works on the groups. This looks like a fault in
  Naemon rather than a rule. (`escalation-exclusions`,
  `dependency-exclusions`, `escalations`, `named-hosts-stay`)
- In a service, a service escalation, a service dependency and a
  `serviceextinfo`, `!` works in the host list, the host group list and the
  description list, on both sides of a dependency, and does take a named
  host out. (`escalation-exclusions`, `dependency-exclusions`,
  `service-expansion`, `named-hosts-stay`)
- A host escalation is made once each time its lists bring a host up. A
  host in two of the named groups, or named and in a named group, or
  written twice, gets the escalation twice. Service escalations,
  dependencies and services are made once. (`hostescalation-repeated-hosts`)
- A service escalation says which services it is about with hosts or host
  groups and a description, with a service group, or with both. A
  description with no host, or a host with no description, is an error.
  (`serviceescalation-sides`, `bad-serviceescalation-description-only`,
  `bad-serviceescalation-no-description` and the cases beside them)
- Each side of a service dependency does the same. One side may give a
  description and nothing else: that means the same host as each service
  of the other side, and a host there that lacks the service is an error.
  Both sides doing so is an error, and a service group with a description
  and no host is one as well. `dependent_host_name null` is the same as
  leaving it out. (`servicedependency-sides`,
  `bad-servicedependency-descriptions-only`,
  `bad-servicedependency-same-host-no-such-service`,
  `bad-servicedependency-group-and-description` and the cases beside them)
- A service dependency with nothing to depend on is accepted and makes
  nothing. One with no dependent service is an error.
  (`servicedependency-sides`, `bad-servicedependency-no-dependent`)
- When a service group is named, `!` works on the whole result.
  `!group` takes out every member of that group, also a service that
  another group or the host and description lists brought in.
  `!description` takes that service out on each host the definition names,
  also where a group brought it in. An excluded host does not reach into
  the service groups. (`servicegroup-exclusions`)
- Lists that are valid and leave nothing: an error for a host escalation, a
  service escalation, a `hostextinfo` and an object's own groups. Accepted,
  with nothing made and nothing printed, for a service, a service
  dependency, a host dependency, a `serviceextinfo` and the `members` of a
  group. (`lists-leave-nothing`, `bad-hostescalation-excluded-away`,
  `bad-serviceescalation-host-excluded-away`, `bad-extinfo-excluded-away`
  and the cases beside them)
- A host dependency that lacks a side is accepted and makes nothing, with
  nothing printed. (`hostdependency-one-sided`)
- A host dependency that names an undefined host or host group is dropped
  whole: Naemon prints "Error: Could not find any host matching" and then
  accepts the configuration, exit 0. The same fault in a service
  dependency refuses it. (`hostdependency-dropped`,
  `bad-servicedependency-partly-unknown`)
- In the members of a service group, `alpha,!load` is accepted and does
  nothing. `!alpha,ping` is an error. (`lists-leave-nothing`,
  `bad-servicegroup-member-excluded-host`)

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
- A timeperiod takes its templates' exceptions as well as its own. An
  exception of its own for the same day replaces the template's. In the
  dump, within each kind, its own come first in the order written, then
  the inherited ones in the reverse of the template's order, and each
  further level of templates turns the order round again.
  (`timeperiod-inherited-exceptions`)

### Regular expressions

- With `use_regexp_matching=1`, `web-*` is a regular expression, not a shell
  wildcard: it means "web" and any number of "-", so it also matches
  `webcam`. (`regexp`)
- Patterns are not anchored: `eb-1.*` matches `web-1` and `web-10`.
  (`regexp`)
- A dotted host name used as a plain name still finds its host. (`regexp`)
- Patterns work in `host_name`, `hostgroup_name`, group `members` and an
  escalation's `service_description`. (`regexp`)
- With regular expressions off, `*` alone stands for every name in each
  of these lists, and in an object's own groups. (`lists-star`)
- With `use_regexp_matching=1`, `*` alone is an error in a list of hosts,
  host groups, service groups or contacts, and in an object's own groups:
  it is read as a regular expression, and it is not a valid one. `.*` does
  the job. For a service description `*` still means every service, until
  `use_true_regexp_matching=1` is set as well. (`bad-regexp-star-host` and
  the cases beside it, `regexp-star-description`,
  `bad-true-regexp-star-description`)
- In a host dependency the same `*` does not refuse the configuration. The
  dependency is dropped, as for an undefined name.
  (`regexp-hostdependency-dropped`)
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

### Extended info

- `hostextinfo` and `serviceextinfo` definitions are accepted with the line
  "WARNING: Extinfo objects are deprecated and will be removed in future
  versions". Their values end up on the host and the service. (`extinfo`)
- Both take `notes`, `notes_url`, `action_url`, `icon_image` and
  `icon_image_alt`. A `hostextinfo` also takes `vrml_image`,
  `statusmap_image`, `2d_coords` and `3d_coords`, and the coordinates are
  checked like a host's. `hostgroup` is read as `hostgroup_name`, and
  `host` is not read as `host_name`. Any other setting, and a custom
  variable, is an error. (`extinfo-settings`, `bad-extinfo-host-setting`,
  `bad-extinfo-service-setting`, `bad-extinfo-custom-variable`,
  `bad-extinfo-coords`, `bad-extinfo-old-host-names`)
- The host's or service's own value stays, wherever the extinfo is read,
  and so does a value from its template. `null` on the host leaves room
  for the extinfo value. Of several extinfo definitions the one read last
  wins, setting by setting, and `null` in an extinfo is no value.
  (`extinfo-precedence`)
- They name their hosts and services with the same lists as an escalation,
  with the differences given under dependencies and escalations above. A
  definition that lacks its hosts, or a `serviceextinfo` that lacks its
  description, is read and does nothing. A
  `serviceextinfo` for a service that one of its hosts lacks is an error.
  (`extinfo-targets`, `extinfo-regexp`,
  `bad-extinfo-service-not-on-every-host`, `bad-extinfo-unknown-service`)
- They have templates of their own, looked up among extinfo definitions
  only. `register 0` does not switch one off: it is applied to the host it
  names all the same. (`extinfo-templates`, `bad-extinfo-template-missing`,
  `bad-extinfo-template-twice`)

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
- The order in which Naemon reads `cfg_file` and `cfg_dir` entries, where
  it decides something. The cases keep each such pair of definitions in
  one file.
- Three or more definitions of one service, beyond the two rows in
  `duplicate-service-mixed`.
- Files that are not UTF-8.
- Regular expressions beyond the simple forms in the regexp cases.
- Very large configs. That belongs to the load benchmark.
- What Naemon does after loading. `initial_state` is one example.
