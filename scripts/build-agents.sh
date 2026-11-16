#!/bin/sh
# Builds checkd-agent for Linux (x86_64 and aarch64, static) and Windows
# (x86_64) in a Rust container, so that nothing needs installing here.
# The binaries land in dist/agent/.
#
#   scripts/build-agents.sh
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
cache=${CHECKD_AGENT_CACHE:-$HOME/.cache/checkd-agent-build}
mkdir -p "$cache/registry" "$cache/target" "$here/dist/agent"
docker run --rm \
  -v "$here":/src:ro \
  -v "$cache/registry":/usr/local/cargo/registry \
  -v "$cache/target":/target \
  -v "$here/dist/agent":/dist \
  -e CARGO_TARGET_DIR=/target \
  -e CHECKD_COMMIT="$(git -C "$here" rev-parse --short=7 HEAD 2>/dev/null || true)" \
  -e DEBIAN_FRONTEND=noninteractive \
  rust:1 sh -eu -c '
apt-get update -qq >/dev/null
apt-get install -y -qq gcc-mingw-w64-x86-64 musl-tools gcc-aarch64-linux-gnu >/dev/null 2>&1
rustup target add x86_64-pc-windows-gnu x86_64-unknown-linux-musl aarch64-unknown-linux-musl >/dev/null 2>&1
export RUSTFLAGS="-C strip=symbols"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-gnu-gcc
export CC_aarch64_unknown_linux_musl=aarch64-linux-gnu-gcc
cd /src
for target in x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-pc-windows-gnu; do
  cargo build --locked --release -p checkd-agent --target $target
done
cp /target/x86_64-unknown-linux-musl/release/checkd-agent /dist/checkd-agent-linux-x86_64
cp /target/aarch64-unknown-linux-musl/release/checkd-agent /dist/checkd-agent-linux-aarch64
cp /target/x86_64-pc-windows-gnu/release/checkd-agent.exe /dist/checkd-agent-windows-x86_64.exe
cd /dist && sha256sum checkd-agent-* > SHA256SUMS
# What they are, as each says it, for knowing which agents are older.
/target/x86_64-unknown-linux-musl/release/checkd-agent --version | sed "s/^checkd-agent //" > VERSION
chown -R '"$(id -u):$(id -g)"' /dist /target
'
ls -l "$here/dist/agent"
