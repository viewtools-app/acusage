#!/usr/bin/env bash
# Re-record the README demo GIFs in docs/demo/ from synthetic logs.
# Needs vhs (https://github.com/charmbracelet/vhs). Usage: scripts/demo/record.sh [name ...]
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

command -v vhs >/dev/null || { echo "vhs not found: brew install vhs" >&2; exit 1; }

cargo build --release --quiet

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
export DEMO_HOME="$work/home"
export DEMO_BIN="$root/target/release"
python3 -I scripts/demo/gen-demo-home.py "$DEMO_HOME"

today="$(TZ=UTC date +%F)"
names=("$@")
if [ ${#names[@]} -eq 0 ]; then
  names=(daily weekly monthly session blocks accounts limits)
fi

for name in "${names[@]}"; do
  tape="$work/$name.tape"
  sed "s/{{TODAY}}/$today/g" "docs/demo/$name.tape" > "$tape"
  echo "recording $name"
  vhs -q "$tape"
done
