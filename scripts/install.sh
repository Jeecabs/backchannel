#!/bin/sh
# Installs the backchannel command. Served by the relay at /install.sh.
#   curl -fsSL __BASE__/install.sh | sh
#   curl -fsSL __BASE__/install.sh | sh -s -- --token bch_...   (also signs in)
set -eu

BASE="__BASE__"
TOKEN=""
while [ $# -gt 0 ]; do
  case "$1" in
    --token) TOKEN="${2:-}"; shift 2 ;;
    *) echo "install.sh: unknown option $1" >&2; exit 2 ;;
  esac
done

case "$(uname -s)" in
  Darwin) os=darwin ;;
  Linux) os=linux ;;
  *) echo "backchannel: no build for $(uname -s) yet." >&2; exit 1 ;;
esac
case "$(uname -m)" in
  arm64|aarch64) arch=arm64 ;;
  x86_64|amd64) arch=x64 ;;
  *) echo "backchannel: no build for $(uname -m) yet." >&2; exit 1 ;;
esac

dir="${BACKCHANNEL_INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$dir"
tmp="$dir/.backchannel.$$"
trap 'rm -f "$tmp"' EXIT
curl -fsSL "$BASE/dl/backchannel-$os-$arch" -o "$tmp"
chmod +x "$tmp"
mv "$tmp" "$dir/backchannel"
echo "Installed backchannel to $dir/backchannel"

if [ -n "$TOKEN" ]; then
  "$dir/backchannel" login --relay "$BASE" --token "$TOKEN"
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
