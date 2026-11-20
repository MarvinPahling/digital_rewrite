#!/bin/sh
# Builds the package and copies the build-generated icon/brand PNGs to a preview directory.
# Usage: assets-preview.sh [PREVIEW_DIR]   (default: <repo>/target/asset-preview)
# The copy happens here, outside build.rs, so Cargo's build-script inputs stay unchanged and
# repeated builds remain fresh. Requires jq.
set -eu

repo=$(cd "$(dirname "$0")/../.." && pwd)
manifest="$repo/reimplementation/Cargo.toml"
preview=${1:-$repo/target/asset-preview}

command -v jq >/dev/null || { echo "error: jq is required" >&2; exit 1; }

out_dir=$(cargo build --locked --manifest-path "$manifest" --message-format=json |
    jq -r 'select(.reason == "build-script-executed" and (.package_id | test("digital-rust"))) | .out_dir' |
    tail -n 1)
[ -n "$out_dir" ] && [ -d "$out_dir/assets" ] || {
    echo "error: build output directory with generated assets not found" >&2
    exit 1
}

rm -rf "$preview"
mkdir -p "$preview"
cp -R "$out_dir/assets/." "$preview/"
echo "Asset previews: $preview"
