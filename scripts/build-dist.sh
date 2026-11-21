#!/usr/bin/env bash
# Builds the backchannel CLI for every platform install.sh offers and puts the
# binaries where the relay serves them (/dl/), with a SHA256SUMS file that
# install.sh checks each download against. Run before `wrangler deploy`.
# Needs: zig, cargo-zigbuild, and the four rust targets below.
#
# Set BACKCHANNEL_DEFAULT_RELAY=https://your-domain to build a CLI that talks
# to your relay when nothing else names one.
set -euo pipefail
cd "$(dirname "$0")/.."
out=viewer/static/dl
mkdir -p "$out"

# Rust records where each source file was when it was compiled. Rewrite those
# paths so the binaries don't carry this machine's home directory.
cargo_home=${CARGO_HOME:-$HOME/.cargo}
rustup_home=${RUSTUP_HOME:-$HOME/.rustup}
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$cargo_home=/cargo --remap-path-prefix=$rustup_home=/rustup --remap-path-prefix=$PWD=/backchannel"

build() { # <rust target> <name> <cargo subcommand>
  cargo "$3" --release -p backchannel --target "$1"
  cp "target/$1/release/backchannel" "$out/backchannel-$2"
}
build aarch64-apple-darwin darwin-arm64 build
build x86_64-apple-darwin darwin-x64 build
build x86_64-unknown-linux-musl linux-x64 zigbuild
build aarch64-unknown-linux-musl linux-arm64 zigbuild

# The checksums install.sh verifies. Written last, from the files as they will be served.
(
  cd "$out"
  rm -f SHA256SUMS
  if command -v sha256sum >/dev/null 2>&1; then sha256sum backchannel-* > SHA256SUMS; else shasum -a 256 backchannel-* > SHA256SUMS; fi
)
ls -lh "$out"
cat "$out/SHA256SUMS"
