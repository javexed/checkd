# checkd

A monitoring engine in Rust that aims to replace the Naemon / Nagios Core
daemon while staying compatible with the Nagios ecosystem: plugins, object
config, external commands, Livestatus and Thruk.

**Status:** the config loader reads an object config. On 127 of the 134
cases in the corpus it agrees with Naemon Core 1.5.2: the same accept or
refuse, and the same objects. The other seven it refuses on purpose where
Naemon accepts. Nothing runs checks yet. The phase 1 gate is not passed: it
calls for real config trees as well as written ones, and no real one has
been tried.

## Try it

    cargo build
    target/debug/checkd verify /path/to/naemon.cfg

`checkd verify` reads the main config and everything it names, prints
warnings and errors, and says whether the config can be used. It exits 0
when it can, 1 when it cannot, and 2 when the command itself was wrong.

    --dump <file>          write the resolved objects in Naemon's precache layout
    --path-map <from>=<to> read files under <from> from <to> instead
    --quiet                print errors only

See [crates/checkd-config/README.md](crates/checkd-config/README.md) for
where the loader differs from Naemon on purpose.

## Design

The design document is kept outside this repo:
(a private page, not published)

Phases, each with a gate that must pass before the next starts:

1. Config loader
2. Checks and state
3. Notifications and operations
4. Livestatus for Thruk
5. Failover
6. Beyond parity

## Layout

- `crates/checkd-config`: the config loader (phase 1).
- `crates/checkd`: the daemon binary.
- `oracle/`: the Naemon test oracle, see [oracle/README.md](oracle/README.md).
- `corpus/`: config cases, each with the oracle's expected output.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

This is a clean-room project. See [CONTRIBUTING.md](CONTRIBUTING.md) before
changing anything.
