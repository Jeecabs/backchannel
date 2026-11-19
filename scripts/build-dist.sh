#!/usr/bin/env bash
# Builds the backchannel CLI for every platform install.sh offers and puts the
# binaries where the relay serves them (/dl/). Run before `wrangler deploy`.
# Needs: zig, cargo-zigbuild, and the four rust targets below.
set -euo pipefail
cd "$(dirname "$0")/.."
out=viewer/static/dl
mkdir -p "$out"
build() { # <rust target> <name> <cargo subcommand>
  cargo "$3" --release -p backchannel --target "$1"
  cp "target/$1/release/backchannel" "$out/backchannel-$2"
}
build aarch64-apple-darwin darwin-arm64 build
build x86_64-apple-darwin darwin-x64 build
build x86_64-unknown-linux-musl linux-x64 zigbuild
build aarch64-unknown-linux-musl linux-arm64 zigbuild
ls -lh "$out"
