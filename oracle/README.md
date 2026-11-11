# Test oracle

A stock Naemon, installed from its binary packages into a container, that the
tests treat as a black box. It answers one question for phase 1: given a
config, what objects does Naemon end up with, and what does it report?

Pinned versions are in the `Dockerfile` (Naemon Core 1.5.2, Livestatus 1.5.1).
Docker is the only requirement.

## Use

    oracle/run <case-dir> <out-dir>     # one case
    oracle/regen [case-name...]         # refresh corpus/<case>/expected

`run` builds the image on first use and writes four files:

| File | Content |
| --- | --- |
| `verify.txt` | output of `naemon -v -p` on the case |
| `verify.exit` | its exit code |
| `objects.precache` | the resolved objects, without the timestamped header; absent when Naemon wrote none |
| `oracle.version` | the Naemon version that produced them |

The container runs without network access and as the calling user.

For phase 2 there is a second question: what does Naemon do on a config
over time?

    oracle/play <case-dir> <out-dir> <seconds>

`play` starts Naemon as a daemon on the case, stops it after the seconds
given, and writes its log, the trace of the scripted plugins' calls, the
moment it began, its status file and the state it saved. Here plugins are
run: the scripts of `runs/lib/plugins`, mounted at `/case/plugins`. The
container has no network and runs as the calling user, as above. See
[`../runs/README.md`](../runs/README.md).

## What a case is

A directory with a main config named `naemon.cfg` and whatever it refers to.

- The case is copied to `/case` in the container, so absolute paths in a case
  start with `/case/`. A relative `cfg_file` is resolved from the main
  config's directory.
- The oracle sets `log_file`, `precached_object_file` and `check_result_path`
  itself and drops any value a case gives for them.
- `run` never runs plugins, so command lines may name paths that don't exist.

In the corpus a case lives at `corpus/<name>/config`, with the oracle's
answer beside it in `corpus/<name>/expected`.

## What was learned by experiment

- `naemon -v -p` writes the precache file without starting the daemon.
- On the packaged sample config the precache file was byte-identical to the
  object cache file a running Naemon writes.
- Objects in it are fully resolved: no templates, no `use`, defaults filled in.
- A config Naemon refuses exits 1 and writes no precache file.

What Naemon did with each config case is in
[`../corpus/README.md`](../corpus/README.md).
