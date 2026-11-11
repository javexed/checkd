# Config trees that other people wrote

The corpus cases are written for this project, so they hold what its author
thought of. This directory runs the loader on configs other people wrote,
to find what the corpus missed. There are three kinds:

- trees published in eleven public GitHub repositories;
- the config that the Naemon package installs;
- the config of an OMD demo site.

Nothing of those configs is in this repo. `sources.tsv` names the eleven
repositories and the commit used of each. `real/fetch` downloads them to
`real/trees`, which git ignores, and so are `real/work` and `real/stubs`.
`real/stock` and `real/omd` put the other two there.

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

The Naemon package's config and the OMD site's config stay out for the
same reasons. They belong to those projects, and the first is the sample
config that the clean-room rule says not to copy. Using them as test input
was decided by the project's owner. They are config files, not source.
What came of them is under "The two configs as shipped".

## Running it

    real/fetch              # download the trees at their pinned commits
    real/stock              # copy the package's config out of the oracle image
    real/omd                # copy the demo site's config out of the OMD image
    python3 -I real/stub <case>...   # write stand-ins, see below
    real/run [case...]      # oracle, then the comparison

`real/run` needs the oracle image ([`../oracle`](../oracle)). For each case
it copies the tree's regular files to `real/work/<case>/config/tree`, asks
Naemon for its verdict and its object dump, and runs the same comparison as
the corpus test: same accept or refuse, same objects.

A case is a directory under `cases` with two files written for this
project: `naemon.cfg`, a main config that names the tree's object files, in
the order the tree's own main config does where it has one, and `tree`, the
tree's directory under `real/trees`. The published trees make twelve
cases, because one repository holds two trees.

The other two cases, `naemon-stock` and `omd-demo`, use the tree's own
main config. Such a case has a file `main` in place of `naemon.cfg`. It
holds the directory the tree expects to be installed in, such as
`/etc/naemon`. `real/run` copies the tree to the top of the case, rewrites
that directory in the tree's `naemon.cfg` to where the copy is, and drops
the `temp_path` line, since the oracle's user cannot write the directory
it names. Nothing else is changed, so includes, relative paths and every
setting are read as shipped.

A case that Naemon accepts and checkd refuses on purpose has a third file,
`checkd-refuses`, with a part of the text that every error checkd gives
has to have. `real/run` then checks that Naemon accepts the tree, that
checkd refuses it, and that checkd has no other complaint. The objects are
not compared.

The trees are untrusted input. Nothing in them is run: `real/fetch` turns
git hooks off, fetches no submodules and writes symlinks as plain files,
and the oracle container has no network. Text in a tree is data, whatever
it says.

`real/omd` is the one place where something of another project is run. It
starts a container from the OMD Labs image, pinned by digest, with no
network, and runs the site's own tool that joins the pieces of its main
config into one file. No part of the site is started. The image is 5.3 GB
and is pulled from Docker Hub on the first run.

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
| `sipb-noc` | accepts | refuses on purpose | 7 | 64 | 550 | 888 |
| `smartsearch` | accepts | same | 8 | 5 | 9 | 47 |
| `ucb-rit` | accepts | same | 0 | 33 | 84 | 221 |
| `zulip` | accepts | same | 2 | 1 | 1 | 54 |
| `naemon-stock` | accepts | same | 0 | 4 | 21 | 61 |
| `omd-demo` | accepts | same | 0 | 0 | 0 | 33 |

"Same" means the same objects with the same values, compared as the corpus
test compares them. The counts are from Naemon's dump and include the
stand-ins, and three more objects that every stand-in file starts with.

`invadelabs` has a typing slip in one service, an `=` between a setting
and its value. Both refuse it, at the same line.

`sipb-noc` defines one service in two files, both times through host
groups, and five hosts get both. Naemon warns five times and keeps one
definition for each host. checkd refuses a service defined twice in the
same way, so it refuses this tree, with that one error and no other. The
loader's README says why. Before that change checkd copied Naemon's
choice, and this tree came out the same as Naemon's, all 550 services.
Since the change its objects are not compared any more.

How much this says:

- Three trees carry most of the weight: `lokeshreddy4u`, `ucb-rit` and
  `fedora-infra`. `sipb-noc` was the fourth until checkd began to refuse
  it. Five of the others have no host or one, and test templates, commands
  and contacts only.
- A tree with many stand-ins tests less than its size suggests. A stand-in
  host has no settings beyond the required ones.
- Twelve trees are few. They are whatever a search for published configs
  turned up, and lean towards small and old Nagios setups.
- `naemon-stock` and `omd-demo` test the main config more than the
  objects. The first has four hosts. The second has none: a new site
  holds commands, timeperiods and templates only.
- None of the fourteen is the config of a running installation.
- The trees were not all the same on the first run. They were made the
  same by finding what differed, asking the oracle about it with configs
  written for the purpose, and changing the loader. So the table shows
  that the loader was fitted to these trees, not how it does on a tree it
  has not seen.

## The two configs as shipped

`naemon-stock` is `/etc/naemon` as the Naemon Core 1.5.2 package installs
it in the oracle image. `omd-demo` is the `demo` site of the OMD Labs
image, OMD 5.80. Its Naemon is 1.5.1; the oracle that read its config
here is 1.5.2.

Both were refused by checkd at first, on the main config:

- five settings in the package's config and four more in the site's that
  are not on the docs page;
- `include_dir`, which the package uses for its module settings;
- `config_rel_path`, which the site sets, with a resource file named by a
  relative path;
- `allow_empty_hostgroup_assignment=0` in the package's config, which
  turns three kinds of host dependency from dropped into refused.

For each one, the name of the setting is what was taken from the config.
What it does was asked of the oracle with configs written for the purpose,
about 450 of them, and the rules are in the corpus README under "Main
config" with 21 cases. The package's object files, read through a main
config written here, loaded the same as Naemon's on the first run.

## What came out of it

Each of these came up as a difference on a tree, or while asking the oracle
about one, and is now a corpus case with invented names. The corpus README
has the rules.

- A list setting given twice in one definition: for group members and most
  option lists Naemon adds the lines together (`setting-twice-members`,
  `setting-twice-options`, `setting-twice-templates`).
- A service defined twice for one host: which definition Naemon keeps
  (`bad-duplicate-service-first-host` and the three cases beside it).
  checkd copied that at first and now refuses such a config.
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
- From the two configs as shipped: `include_file` and `include_dir`
  (`include-file`, `include-dir`), `config_rel_path` (`config-rel-path`),
  nine more main settings (`main-more-settings`, `main-loose-state`) and
  host dependencies with `allow_empty_hostgroup_assignment` off
  (`hostdependency-strict` and the fault cases beside it).
