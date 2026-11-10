# checkd

A monitoring engine in Rust that aims to replace the Naemon / Nagios Core
daemon while staying compatible with the Nagios ecosystem: plugins, object
config, external commands, Livestatus and Thruk.

**Status:** nothing is implemented yet. Work starts with phase 1, the config
loader.

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

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

This is a clean-room project. See [CONTRIBUTING.md](CONTRIBUTING.md) before
changing anything.
