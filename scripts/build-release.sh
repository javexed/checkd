#!/bin/sh
# The files of a release, in dist/release/: checkd for Linux (x86_64 and
# aarch64, static, every feature), the agents as nuthatch serves them
# (scripts/build-agents.sh, packed with their SHA256SUMS and VERSION),
# and a SHA256SUMS of it all. Built in a Rust container, as the agents
# are, so that nothing needs installing here. The release workflow runs
# this on a tag; it runs the same by hand.
#
#   scripts/build-release.sh
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
cache=${CHECKD_AGENT_CACHE:-$HOME/.cache/checkd-agent-build}
out=$here/dist/release
rm -rf "$out"
mkdir -p "$out" "$cache/registry" "$cache/target"

"$here/scripts/build-agents.sh"
# The version of Cargo.toml, as the tag has it (no commit: a "+" is not
# kept in a release file's name).
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$here/Cargo.toml" | head -1)
tar -C "$here/dist" -czf "$out/checkd-agents-$version.tar.gz" agent

docker run --rm \
  -v "$here":/src:ro \
  -v "$cache/registry":/usr/local/cargo/registry \
  -v "$cache/target":/target \
  -v "$out":/out \
  -e CARGO_TARGET_DIR=/target \
  -e CHECKD_COMMIT="$(git -C "$here" rev-parse --short=7 HEAD 2>/dev/null || true)" \
  -e DEBIAN_FRONTEND=noninteractive \
  rust:1 sh -eu -c '
# zig compiles the bundled SQLite against musl for both targets; the
# Debian cross compiler would bring glibc headers to aarch64.
apt-get update -qq >/dev/null
apt-get install -y -qq python3-pip >/dev/null 2>&1
pip install -q --root-user-action=ignore --break-system-packages ziglang==0.13.0
cargo install -q --locked cargo-zigbuild@0.24.1
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl >/dev/null 2>&1
export RUSTFLAGS="-C strip=symbols"
cd /src
for target in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
  cargo zigbuild --locked --release -p checkd --all-features --target $target
done
cp /target/x86_64-unknown-linux-musl/release/checkd /out/checkd-linux-x86_64
cp /target/aarch64-unknown-linux-musl/release/checkd /out/checkd-linux-aarch64
chown -R '"$(id -u):$(id -g)"' /out /target
'
for arch in x86_64 aarch64; do mv "$out/checkd-linux-$arch" "$out/checkd-$version-linux-$arch"; done
(cd "$out" && sha256sum checkd-* > SHA256SUMS)
ls -l "$out"
