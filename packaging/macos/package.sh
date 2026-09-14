#!/usr/bin/env bash
# Packages squint for macOS: one universal squint.app, for Apple silicon and
# Intel both, in a disk image, and the bare binary in a tarball.
#
#   packaging/macos/package.sh <version> <arm64-binary> <x86_64-binary> <out-dir>
#
# The app is signed ad hoc, not with a Developer ID, so a downloaded copy is
# quarantined: open it with a right click → Open the first time.
set -euo pipefail

version="$1"
arm="$2"
intel="$3"
out="$(mkdir -p "$4" && cd "$4" && pwd)"
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# CFBundleVersion takes digits and dots only.
bundle_version="${version%%-*}"

app="$work/squint.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create -output "$app/Contents/MacOS/squint" "$arm" "$intel"
cp "$root/packaging/icons/squint.icns" "$app/Contents/Resources/squint.icns"
sed -e "s/@VERSION@/$version/g" -e "s/@BUNDLE_VERSION@/$bundle_version/g" \
  "$here/Info.plist" > "$app/Contents/Info.plist"
codesign --force --deep --sign - "$app"

name="squint-$version-macos-universal"

dmg_root="$work/dmg"
mkdir -p "$dmg_root"
cp -R "$app" "$dmg_root/"
ln -s /Applications "$dmg_root/Applications"
hdiutil create -volname "squint $version" -srcfolder "$dmg_root" -ov -format UDZO "$out/$name.dmg"

mkdir -p "$work/$name"
cp "$app/Contents/MacOS/squint" "$root/LICENSE" "$root/README.md" "$work/$name/"
tar -C "$work" -czf "$out/$name.tar.gz" "$name"

ls -la "$out"
