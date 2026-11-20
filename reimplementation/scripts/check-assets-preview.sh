#!/bin/sh
# Regression check: preview generation must not invalidate Cargo's build-script inputs, and
# a deleted preview directory must be recreated with every generated PNG.
set -eu

repo=$(cd "$(dirname "$0")/../.." && pwd)
manifest="$repo/reimplementation/Cargo.toml"
script="$repo/reimplementation/scripts/assets-preview.sh"
preview=$(mktemp -d)/asset-preview
trap 'rm -rf "$(dirname "$preview")"' EXIT

expected=59

"$script" "$preview" >/dev/null
first=$(find "$preview" -name '*.png' | wc -l | tr -d ' ')
[ "$first" -eq "$expected" ] || { echo "FAIL: $first PNGs after first run, expected $expected" >&2; exit 1; }

# A repeated preview run must not recompile.
log=$("$script" "$preview" 2>&1 >/dev/null)
case "$log" in
*Compiling*) echo "FAIL: repeated preview run recompiled:" >&2; echo "$log" >&2; exit 1 ;;
esac

# A plain build must be fresh: Cargo prints no "Compiling" line.
log=$(cargo build --locked --manifest-path "$manifest" 2>&1)
case "$log" in
*Compiling*) echo "FAIL: build after preview generation recompiled:" >&2; echo "$log" >&2; exit 1 ;;
esac

rm -rf "$preview"
"$script" "$preview" >/dev/null
again=$(find "$preview" -name '*.png' | wc -l | tr -d ' ')
[ "$again" -eq "$expected" ] || { echo "FAIL: $again PNGs after recreation, expected $expected" >&2; exit 1; }

log=$(cargo build --locked --manifest-path "$manifest" 2>&1)
case "$log" in
*Compiling*) echo "FAIL: build after recreation recompiled:" >&2; echo "$log" >&2; exit 1 ;;
esac
echo "assets-preview check passed"
