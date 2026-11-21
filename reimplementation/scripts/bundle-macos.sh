#!/bin/bash
# Builds a macOS app bundle for the native editor (host architecture only).
# Usage: bundle-macos.sh [BUNDLE_DIR]   (default: <repo>/target/bundle/digital-rust.app)
# Environment: CODESIGN_IDENTITY  signing identity for codesign (default "-" = ad-hoc).
#   BUNDLE_MACOS_TEST_FAIL_INSTALL  test hook, inert unless set to a non-empty value: makes the final
#   install step fail after the previous bundle was moved aside, to exercise the rollback.
# The app icon is derived here from the build-generated app-icon PNGs (never committed) via
# an .iconset and `iconutil`. Cargo writes to reimplementation/target; the paths of the binary
# and the generated assets are taken from Cargo's JSON build messages, not guessed. Requires jq.
set -euo pipefail

[ "$(uname -s)" = Darwin ] || { echo "error: bundle-macos.sh only runs on macOS" >&2; exit 1; }
for tool in jq iconutil codesign plutil; do
    command -v "$tool" >/dev/null || { echo "error: $tool is required" >&2; exit 1; }
done

repo=$(cd "$(dirname "$0")/../.." && pwd)
manifest="$repo/reimplementation/Cargo.toml"
bundle=${1:-$repo/target/bundle/digital-rust.app}
identity=${CODESIGN_IDENTITY:--}

# Provisional values: the application name and identifier are working names.
name=digital-rust
identifier=io.github.digital-rust.editor
min_macos=11.0

work=$(mktemp -d)
# Installation transaction directory: a temporary sibling of the destination (same filesystem, so the
# moves below are atomic renames). It holds the staged bundle and, while installing, the previous one.
txn=
cleanup() {
    trap - EXIT INT TERM HUP
    # The previous bundle was moved aside but the replacement was not installed: put it back.
    # -L as well as -e: a relative symlink bundle dangles once moved into $txn.
    if [ -n "$txn" ] && { [ -e "$txn/previous" ] || [ -L "$txn/previous" ]; } && [ ! -e "$bundle" ] && [ ! -L "$bundle" ]; then
        if ! mv "$txn/previous" "$bundle"; then
            echo "error: could not restore the previous bundle; it remains at $txn/previous" >&2
            rm -rf "$work"
            return
        fi
    fi
    [ -z "$txn" ] || rm -rf "$txn"
    rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

version=$(cargo metadata --locked --no-deps --format-version 1 --manifest-path "$manifest" |
    jq -r --arg n "$name" '.packages[] | select(.name == $n) | .version')
[ -n "$version" ] || { echo "error: version of $name not found in $manifest" >&2; exit 1; }

cargo build --release --locked --manifest-path "$manifest" --message-format=json >"$work/build.json"
binary=$(jq -r --arg n "$name" 'select(.reason == "compiler-artifact" and .target.name == $n and (.target.kind | index("bin"))) | .executable // empty' "$work/build.json" | tail -n 1)
out_dir=$(jq -r 'select(.reason == "build-script-executed" and (.package_id | test("digital-rust"))) | .out_dir' "$work/build.json" | tail -n 1)
[ -n "$binary" ] && [ -x "$binary" ] || { echo "error: release binary not found" >&2; exit 1; }
[ -n "$out_dir" ] && [ -d "$out_dir/assets/brand" ] || { echo "error: generated brand assets not found" >&2; exit 1; }

# Iconset: 512@2x would need a 1024 px image, which the build does not generate, so it is omitted.
iconset="$work/AppIcon.iconset"
mkdir "$iconset"
while read -r file px; do
    src="$out_dir/assets/brand/app-icon-$px.png"
    [ -f "$src" ] || { echo "error: missing generated icon $src" >&2; exit 1; }
    cp "$src" "$iconset/$file"
done <<'SIZES'
icon_16x16.png 16
icon_16x16@2x.png 32
icon_32x32.png 32
icon_32x32@2x.png 64
icon_128x128.png 128
icon_128x128@2x.png 256
icon_256x256.png 256
icon_256x256@2x.png 512
icon_512x512.png 512
SIZES
iconutil -c icns "$iconset" -o "$work/AppIcon.icns"

# Assemble next to the destination, then swap, so a failed run keeps the previous bundle.
mkdir -p "$(dirname "$bundle")"
txn=$(mktemp -d "$(dirname "$bundle")/.$name-install.XXXXXX")
staged="$txn/$(basename "$bundle")"
mkdir -p "$staged/Contents/MacOS" "$staged/Contents/Resources"
cp "$binary" "$staged/Contents/MacOS/$name"
chmod 755 "$staged/Contents/MacOS/$name"
cp "$work/AppIcon.icns" "$staged/Contents/Resources/AppIcon.icns"
cat >"$staged/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>$name</string>
    <key>CFBundleDisplayName</key><string>$name</string>
    <key>CFBundleIdentifier</key><string>$identifier</string>
    <key>CFBundleExecutable</key><string>$name</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>$min_macos</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
</dict>
</plist>
PLIST
plutil -lint "$staged/Contents/Info.plist" >/dev/null

codesign --force --deep --sign "$identity" "$staged"
codesign --verify --deep --strict "$staged"

# Install: move the previous bundle aside, move the staged one in; cleanup restores the previous
# bundle if either step fails or the run is interrupted, and discards it only after success.
if [ -e "$bundle" ] || [ -L "$bundle" ]; then
    mv "$bundle" "$txn/previous"
fi
if [ -n "${BUNDLE_MACOS_TEST_FAIL_INSTALL:-}" ]; then
    echo "error: injected install failure (BUNDLE_MACOS_TEST_FAIL_INSTALL)" >&2
    exit 1
fi
mv "$staged" "$bundle"
echo "App bundle: $bundle (signed with identity '$identity', host architecture only)"
