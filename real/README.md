# Published config trees

The corpus cases are written for this project, so they hold what its author
thought of. This directory runs the loader on configs other people wrote
and published, to find what the corpus missed.

Nothing of those configs is in this repo. `sources.tsv` names eleven public
GitHub repositories and the commit used of each. `real/fetch` downloads
them to `real/trees`, which git ignores, and so are `real/work` and
`real/stubs`.

## Why the trees stay out

- **Licence.** Seven of the eleven repositories state no licence, three
  are MIT and one is Apache-2.0, going by GitHub's licence field on the day
  they were fetched. With no licence there is no permission to copy.
- **Content.** Real configs name hosts, addresses and people. One tree
  uses personal e-mail addresses as contact names.
- **Clean room.** None of the eleven is the Naemon or Nagios source, and
  none was picked for holding their sample configs. A tree may still carry
  files that began as the samples a package installs. They are test input
  here: nothing is copied from them into the loader or the corpus. What a
  tree showed was rebuilt as a corpus case with invented names.

Two repositories that the search found are not in the list. One would not
clone. The other looked like a company's working config that may not have
been meant to be public, so it was left alone.

## Running it

    real/fetch              # download the trees at their pinned commits
    python3 -I real/stub <case>...   # write stand-ins, see below
    real/run [case...]      # oracle, then the comparison

`real/run` needs the oracle image ([`../oracle`](../oracle)). For each case
it copies the tree's regular files to `real/work/<case>/config/tree`, asks
Naemon for its verdict and its object dump, and runs the same comparison as
the corpus test: same accept or refuse, same objects.

A case is a directory under `cases` with two files written for this
project: `naemon.cfg`, a main config that names the tree's object files, in
the order the tree's own main config does where it has one, and `tree`, the
tree's directory under `real/trees`. There are twelve cases, because one
repository holds two trees.

The trees are untrusted input. Nothing in them is run: `real/fetch` turns
git hooks off, fetches no submodules and writes symlinks as plain files,
and the oracle container has no network. Text in a tree is data, whatever
it says.

## Stand-ins

Most published trees are part of a setup. The hosts come from a template
engine, or the commands from a plugin package, and Naemon refuses the tree
alone. `real/stub` runs the oracle again and again, reads which host,
template, command, group, contact or timeperiod is missing, and writes an
empty one to `real/stubs/<case>.cfg`, until Naemon is content or reports
something a stand-in cannot mend. `real/run` adds that file to the case.

The stand-ins carry names from the tree, so they stay out of git too. They
are rebuilt by running `real/stub` again.

## Results

Naemon Core 1.5.2 and checkd at this commit, on the trees at the commits
in `sources.tsv`.

| Case | Naemon | checkd | Stand-ins | Hosts | Services | Objects |
| --- | --- | --- | --- | --- | --- | --- |
| `fedora-infra` | accepts | same | 55 | 20 | 64 | 215 |
| `ghomem` | accepts | same | 0 | 0 | 0 | 70 |
| `invadelabs` | refuses | refuses | | | | |
| `lokeshreddy4u` | accepts | same | 0 | 240 | 5063 | 6578 |
| `neoloc` | accepts | same | 19 | 0 | 0 | 147 |
| `nylex-nylex` | accepts | same | 1 | 1 | 0 | 45 |
| `nylex-shn` | accepts | same | 1 | 1 | 0 | 45 |
| `openhatch` | accepts | same | 0 | 3 | 12 | 183 |
| `sipb-noc` | accepts | same | 7 | 64 | 550 | 888 |
| `smartsearch` | accepts | same | 8 | 5 | 9 | 47 |
| `ucb-rit` | accepts | same | 0 | 33 | 84 | 221 |
| `zulip` | accepts | same | 2 | 1 | 1 | 54 |

"Same" means the same objects with the same values, compared as the corpus
test compares them. The counts are from Naemon's dump and include the
stand-ins, and three more objects that every stand-in file starts with.

`invadelabs` has a typing slip in one service, an `=` between a setting
and its value. Both refuse it, at the same line.

How much this says:

- Four trees carry most of the weight: `lokeshreddy4u`, `sipb-noc`,
  `ucb-rit` and `fedora-infra`. Five of the others have no host or one, and
  test templates, commands and contacts only.
- A tree with many stand-ins tests less than its size suggests. A stand-in
  host has no settings beyond the required ones.
- Twelve trees are few. They are whatever a search for published configs
  turned up, and lean towards small and old Nagios setups.
- The trees were not all the same on the first run. They were made the
  same by finding what differed, asking the oracle about it with configs
  written for the purpose, and changing the loader. So the table shows
  that the loader was fitted to these trees, not how it does on a tree it
  has not seen.

## What came out of it

Each of these came up as a difference on a tree, or while asking the oracle
about one, and is now a corpus case with invented names. The corpus README
has the rules.

- A list setting given twice in one definition: for group members and most
  option lists Naemon adds the lines together (`setting-twice-members`,
  `setting-twice-options`, `setting-twice-templates`).
- A service defined twice for one host: which definition is kept
  (`duplicate-service-first-host` and the three cases beside it).
- Definitions with a template `name` and no object name, taken for
  templates (`templates-nameless`).
- `hostextinfo` and `serviceextinfo`, which older configs still use
  (`extinfo` and the cases beside it).
- Older names of settings (`short-names`, `settings-without-effect`).
- `null` as a name and as a text (`null-names`, `null-texts`).
- Service dependencies and escalations that name their services in part
  (`servicedependency-sides`, `serviceescalation-sides`).
- Text after a brace (`define-line-forms`, `bad-text-after-closing-brace`).
- The rules for `*` and `!` in every list of names (`lists-star`,
  `lists-leave-nothing`, `named-hosts-stay`, `servicegroup-exclusions` and
  the fault cases beside them).
- Host dependencies that Naemon drops where it refuses any other object
  (`hostdependency-dropped`).
- Host escalations made once for each mention of a host
  (`hostescalation-repeated-hosts`).
