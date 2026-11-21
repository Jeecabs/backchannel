#!/usr/bin/env bash
# Checks install.sh against a throwaway site on localhost. It must install a
# download that matches its checksum, and install nothing when the download was
# changed, when there is no checksum for it, or when the script itself arrives
# cut off. The "binary" is a stand-in that prints how it was called.
set -euo pipefail
cd "$(dirname "$0")/.."
port=${1:-8791}
base="http://127.0.0.1:$port"
work=$(mktemp -d)
server=""
trap 'if [ -n "$server" ]; then kill "$server" 2>/dev/null; wait "$server" 2>/dev/null || true; fi; rm -rf "$work"' EXIT

case "$(uname -s)" in Darwin) os=darwin ;; *) os=linux ;; esac
case "$(uname -m)" in arm64|aarch64) arch=arm64 ;; *) arch=x64 ;; esac
file="backchannel-$os-$arch"

mkdir -p "$work/site/dl" "$work/home"
sed "s#__BASE__#$base#g" scripts/install.sh > "$work/site/install.sh"
printf '#!/bin/sh\necho "ran: $* token=${BACKCHANNEL_TOKEN:-none}"\n' > "$work/good"
sum() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
publish() { # <file to serve> [file whose checksum is listed: the same one unless given]
  cp "$1" "$work/site/dl/$file"
  mkdir -p "$work/listed"
  cp "${2:-$1}" "$work/listed/$file"
  (cd "$work/listed" && sum "$file" > "$work/site/dl/SHA256SUMS")
}
python3 -m http.server "$port" --bind 127.0.0.1 --directory "$work/site" >/dev/null 2>&1 &
server=$!
curl -s --retry 20 --retry-connrefused --retry-delay 1 -o /dev/null "$base/install.sh"

# Runs the installer the way a person would, with nothing of this machine's in the way.
install() { rm -rf "$work/bin"; curl -fsSL "$base/install.sh" | env -i HOME="$work/home" PATH="$PATH" BACKCHANNEL_INSTALL_DIR="$work/bin" "$@" 2>&1; }
ok() { echo "ok: $1"; }
bad() { echo "FAIL: $1"; echo "$2"; exit 1; }
installed() { [ -x "$work/bin/backchannel" ]; }
clean() { [ -z "$(ls -A "$work/bin" 2>/dev/null)" ]; }

publish "$work/good"
out=$(install sh) && installed || bad "a download that matches its checksum should install" "$out"
grep -q "create <org name> --relay $base" <<<"$out" || bad "the closing hints should name this relay" "$out"
ok "installs a download that matches its checksum, and points at the relay it came from"

out=$(install env BACKCHANNEL_TOKEN=bch_from_env sh) && grep -q "ran: login --relay $base token=bch_from_env" <<<"$out" || bad "a token in the environment should sign in" "$out"
out=$(install sh -s -- --token bch_from_flag) && grep -q "ran: login --relay $base token=bch_from_flag" <<<"$out" || bad "--token should sign in" "$out"
ok "takes the token from BACKCHANNEL_TOKEN or --token, and hands it to the CLI in the environment"

printf '#!/bin/sh\necho "ran: something else entirely"\n' > "$work/evil"
publish "$work/evil" "$work/good"
if out=$(install sh); then bad "a changed download was installed" "$out"; fi
clean || bad "a refused download left files behind" "$(ls -A "$work/bin")"
grep -q "doesn't match its checksum" <<<"$out" || bad "the refusal should say why" "$out"
ok "refuses a download that doesn't match its checksum, and leaves nothing behind"

publish "$work/good"
echo "0000000000000000000000000000000000000000000000000000000000000000  backchannel-other-os" > "$work/site/dl/SHA256SUMS"
if out=$(install sh); then bad "installed with no checksum for this platform" "$out"; fi
clean || bad "left files behind" "$(ls -A "$work/bin")"
rm "$work/site/dl/SHA256SUMS"
if out=$(install sh); then bad "installed with no checksum list at all" "$out"; fi
clean || bad "left files behind" "$(ls -A "$work/bin")"
ok "refuses to install when there is no checksum to check against"

# A script cut off anywhere before its last line must do nothing at all.
publish "$work/good"
size=$(wc -c < "$work/site/install.sh")
n=0
for cut in $(seq 40 61 $((size - 12))); do
  rm -rf "$work/bin"
  head -c "$cut" "$work/site/install.sh" | env -i HOME="$work/home" PATH="$PATH" BACKCHANNEL_INSTALL_DIR="$work/bin" sh >/dev/null 2>&1 || true
  [ ! -e "$work/bin" ] || bad "a script cut off at byte $cut still did something" "$(ls -A "$work/bin")"
  n=$((n + 1))
done
ok "does nothing when the script arrives cut off ($n cut points tried)"

echo
echo "All installer checks passed."
