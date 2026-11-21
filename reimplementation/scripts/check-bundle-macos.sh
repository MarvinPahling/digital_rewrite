#!/bin/bash
# Regression check: builds the macOS bundle, asserts its structure and signature, and that a failed
# install keeps the previous bundle and leaves no staging directory behind.
set -euo pipefail

[ "$(uname -s)" = Darwin ] || { echo "error: check-bundle-macos.sh only runs on macOS" >&2; exit 1; }

repo=$(cd "$(dirname "$0")/../.." && pwd)
script="$repo/reimplementation/scripts/bundle-macos.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
bundle="$tmp/digital-rust.app"

fail() { echo "FAIL: $*" >&2; exit 1; }

"$script" "$bundle" >/dev/null
# A second run must replace the bundle; a stale file must not survive it.
touch "$bundle/Contents/stale"
"$script" "$bundle" >/dev/null
[ ! -e "$bundle/Contents/stale" ] || fail "second run did not replace the previous bundle"

# A failing final install step must roll back: the previous bundle stays intact and nothing is left over.
echo previous >"$bundle/Contents/rollback-marker"
if BUNDLE_MACOS_TEST_FAIL_INSTALL=1 "$script" "$bundle" >/dev/null 2>"$tmp/fail.log"; then
    fail "injected install failure did not fail the script"
fi
grep -q 'injected install failure' "$tmp/fail.log" || fail "install failure was not the injected one"
[ "$(cat "$bundle/Contents/rollback-marker" 2>/dev/null)" = previous ] || fail "previous bundle was not restored"
[ -x "$bundle/Contents/MacOS/digital-rust" ] || fail "restored bundle lost its executable"
rm -f "$tmp/fail.log"
[ "$(ls -A "$tmp")" = digital-rust.app ] || fail "leftover staging or backup entries: $(ls -A "$tmp")"
# A successful run afterwards replaces it and leaves nothing behind either.
"$script" "$bundle" >/dev/null
[ ! -e "$bundle/Contents/rollback-marker" ] || fail "run after the failed install did not replace the bundle"
[ "$(ls -A "$tmp")" = digital-rust.app ] || fail "leftover entries after success: $(ls -A "$tmp")"

# A previous bundle that is a relative symlink dangles while moved aside; rollback must still restore it.
linkdir="$tmp/linkdest"
mkdir -p "$linkdir/releases/current.app"
echo target >"$linkdir/releases/current.app/marker"
ln -s releases/current.app "$linkdir/digital-rust.app"
if BUNDLE_MACOS_TEST_FAIL_INSTALL=1 "$script" "$linkdir/digital-rust.app" >/dev/null 2>"$tmp/fail.log"; then
    fail "injected install failure did not fail the script (symlink bundle)"
fi
rm -f "$tmp/fail.log"
[ -L "$linkdir/digital-rust.app" ] || fail "symlink bundle was not restored"
[ "$(readlink "$linkdir/digital-rust.app")" = releases/current.app ] || fail "restored symlink target changed"
[ "$(cat "$linkdir/digital-rust.app/marker" 2>/dev/null)" = target ] || fail "restored symlink does not resolve"
[ "$(ls -A "$linkdir" | sort | tr '\n' ' ')" = "digital-rust.app releases " ] || fail "leftover entries beside symlink bundle: $(ls -A "$linkdir")"
rm -rf "$linkdir"

exe="$bundle/Contents/MacOS/digital-rust"
[ -x "$exe" ] || fail "executable bit missing on $exe"
plutil -lint "$bundle/Contents/Info.plist" >/dev/null || fail "Info.plist is invalid"
plist() { plutil -extract "$1" raw -o - "$bundle/Contents/Info.plist"; }
[ "$(plist CFBundleExecutable)" = digital-rust ] || fail "CFBundleExecutable"
[ "$(plist CFBundlePackageType)" = APPL ] || fail "CFBundlePackageType"
[ "$(plist CFBundleIconFile)" = AppIcon ] || fail "CFBundleIconFile"
[ "$(plist NSHighResolutionCapable)" = true ] || fail "NSHighResolutionCapable"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/reimplementation/Cargo.toml" | head -n 1)
[ "$(plist CFBundleShortVersionString)" = "$version" ] || fail "CFBundleShortVersionString != $version"
[ "$(plist CFBundleVersion)" = "$version" ] || fail "CFBundleVersion != $version"

icns="$bundle/Contents/Resources/AppIcon.icns"
[ -s "$icns" ] || fail "AppIcon.icns missing"
iconutil --convert iconset "$icns" -o "$tmp/round.iconset" || fail "AppIcon.icns is not a valid icon"
for f in icon_16x16.png icon_16x16@2x.png icon_32x32.png icon_32x32@2x.png icon_128x128.png \
    icon_128x128@2x.png icon_256x256.png icon_256x256@2x.png icon_512x512.png; do
    [ -s "$tmp/round.iconset/$f" ] || fail "icon round trip lost $f"
done
[ "$(sips -g pixelWidth "$tmp/round.iconset/icon_512x512.png" | awk '/pixelWidth/ {print $2}')" = 512 ] ||
    fail "512 px icon has the wrong width"

codesign --verify --deep --strict "$bundle" || fail "codesign verification failed"

# Launch smoke test without a display: the binary is a Mach-O that links its libraries.
file "$exe" | grep -q 'Mach-O' || fail "$exe is not a Mach-O executable"
lipo -info "$exe" >/dev/null || fail "lipo cannot read $exe"
otool -L "$exe" | grep -q 'libSystem' || fail "otool shows no libSystem link"

echo "bundle-macos check passed"
