#!/bin/sh
# Installs the backchannel command. Served by the relay at /install.sh.
#   curl -fsSL __BASE__/install.sh | sh
#   curl -fsSL __BASE__/install.sh | BACKCHANNEL_TOKEN=bch_... sh    (also signs in)
#   curl -fsSL __BASE__/install.sh | sh -s -- --token bch_...        (the same)
#
# Everything happens inside main, which is called on the last line. If the
# download of this script is cut off part way, nothing runs.
set -eu

fail() {
  echo "backchannel: $1" >&2
  exit 1
}

# Prints the SHA-256 of a file, with whichever tool this machine has.
sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | awk '{ print $NF }'
  else
    fail "no sha256sum, shasum or openssl here, so the download can't be checked. Not installing."
  fi
}

main() {
  BASE="__BASE__"
  TOKEN="${BACKCHANNEL_TOKEN:-}"
  while [ $# -gt 0 ]; do
    case "$1" in
      --token) TOKEN="${2:-}"; shift 2 ;;
      *) echo "install.sh: unknown option $1" >&2; exit 2 ;;
    esac
  done

  case "$(uname -s)" in
    Darwin) os=darwin ;;
    Linux) os=linux ;;
    *) fail "no build for $(uname -s) yet." ;;
  esac
  case "$(uname -m)" in
    arm64|aarch64) arch=arm64 ;;
    x86_64|amd64) arch=x64 ;;
    *) fail "no build for $(uname -m) yet." ;;
  esac
  file="backchannel-$os-$arch"

  dir="${BACKCHANNEL_INSTALL_DIR:-$HOME/.local/bin}"
  mkdir -p "$dir"
  tmp="$dir/.backchannel.$$"
  sums="$dir/.backchannel.sums.$$"
  trap 'rm -f "$tmp" "$sums"' EXIT
  curl -fsSL "$BASE/dl/$file" -o "$tmp" || fail "couldn't download $BASE/dl/$file"

  # The relay publishes the SHA-256 of each binary it serves. A download that
  # doesn't match is thrown away: nothing unchecked is ever made executable.
  curl -fsSL "$BASE/dl/SHA256SUMS" -o "$sums" || fail "$BASE has no checksum list (dl/SHA256SUMS), so the download can't be checked. Not installing."
  want=$(awk -v f="$file" '$2 == f || $2 == "*" f { print $1; exit }' "$sums")
  [ -n "$want" ] || fail "$BASE/dl/SHA256SUMS has no entry for $file. Not installing."
  got=$(sha256 "$tmp")
  [ "$got" = "$want" ] || fail "the download of $file doesn't match its checksum (expected $want, got $got). Not installing."

  chmod +x "$tmp"
  mv "$tmp" "$dir/backchannel"
  echo "Installed backchannel to $dir/backchannel (sha256 $got)"

  if [ -n "$TOKEN" ]; then
    # Handed over in the environment, so it never shows in the process list.
    BACKCHANNEL_TOKEN="$TOKEN" "$dir/backchannel" login --relay "$BASE"
  fi

  case ":$PATH:" in
    *":$dir:"*) ;;
    *) echo
       echo "$dir is not on your PATH. Add this line to your shell profile:"
       echo "  export PATH=\"$dir:\$PATH\"" ;;
  esac

  if [ -z "$TOKEN" ]; then
    echo
    echo "Next: backchannel join <your invite link>"
    echo "  or: backchannel join <invite code> --relay $BASE"
    echo "  or: backchannel create <org name> --relay $BASE   to start a new org"
  fi
}

main "$@"
