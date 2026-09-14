#!/usr/bin/env bash
# Packages squint for macOS: one universal squint.app, for Apple silicon and
# Intel both, in a disk image, and the bare binary in a tarball.
#
#   packaging/macos/package.sh <version> <arm64-binary> <x86_64-binary> <out-dir>
#
# Signing is chosen by the environment:
#
#   MACOS_SIGN_IDENTITY   a Developer ID Application identity in a keychain;
#                         without it the app is signed ad hoc, and a
#                         downloaded copy is stopped by Gatekeeper
#   APPLE_ID, APPLE_APP_PASSWORD, APPLE_TEAM_ID
#                         with the identity, the app and the disk image are
#                         notarized by Apple and the tickets stapled to them
set -euo pipefail

version="$1"
arm="$2"
intel="$3"
out="$(mkdir -p "$4" && cd "$4" && pwd)"
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

identity="${MACOS_SIGN_IDENTITY:-}"
notarize=""
if [ -n "$identity" ] && [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_APP_PASSWORD:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; then
  notarize=1
fi

# Sends a file to Apple's notary service and waits for its verdict; on a
# rejection, prints Apple's log of why before failing.
notarize() {
  local file="$1" result id status
  result="$(xcrun notarytool submit "$file" \
    --apple-id "$APPLE_ID" --password "$APPLE_APP_PASSWORD" --team-id "$APPLE_TEAM_ID" \
    --wait --timeout 30m --output-format json)"
  echo "$result"
  id="$(plutil -extract id raw -o - - <<< "$result")"
  status="$(plutil -extract status raw -o - - <<< "$result")"
  if [ "$status" != Accepted ]; then
    xcrun notarytool log "$id" \
      --apple-id "$APPLE_ID" --password "$APPLE_APP_PASSWORD" --team-id "$APPLE_TEAM_ID" || true
    echo "notarization of $(basename "$file") ended $status" >&2
    return 1
  fi
}

# CFBundleVersion takes digits and dots only.
bundle_version="${version%%-*}"

app="$work/squint.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create -output "$app/Contents/MacOS/squint" "$arm" "$intel"
cp "$root/packaging/icons/squint.icns" "$app/Contents/Resources/squint.icns"
sed -e "s/@VERSION@/$version/g" -e "s/@BUNDLE_VERSION@/$bundle_version/g" \
  "$here/Info.plist" > "$app/Contents/Info.plist"

if [ -n "$identity" ]; then
  # Notarization wants the hardened runtime and a secure timestamp. The app
  # is one executable, so signing the bundle signs everything in it.
  codesign --force --options runtime --timestamp --sign "$identity" "$app"
else
  echo "MACOS_SIGN_IDENTITY is not set: signing ad hoc" >&2
  codesign --force --sign - "$app"
fi
codesign --verify --strict --verbose=2 "$app"

if [ -n "$notarize" ]; then
  # The app is notarized on its own so its ticket can be stapled inside the
  # disk image, where it is found offline.
  ditto -c -k --keepParent "$app" "$work/squint.zip"
  notarize "$work/squint.zip"
  xcrun stapler staple "$app"
elif [ -n "$identity" ]; then
  echo "APPLE_ID, APPLE_APP_PASSWORD or APPLE_TEAM_ID is not set: not notarizing" >&2
fi

name="squint-$version-macos-universal"
dmg="$out/$name.dmg"

dmg_root="$work/dmg"
mkdir -p "$dmg_root"
cp -R "$app" "$dmg_root/"
ln -s /Applications "$dmg_root/Applications"
hdiutil create -volname "squint $version" -srcfolder "$dmg_root" -ov -format UDZO "$dmg"

if [ -n "$identity" ]; then
  codesign --force --timestamp --sign "$identity" "$dmg"
fi
if [ -n "$notarize" ]; then
  notarize "$dmg"
  xcrun stapler staple "$dmg"
  spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"
  spctl --assess --type execute --verbose=2 "$app"
fi

# The bare binary is the app's, signature and all: a lone executable cannot
# hold a stapled ticket, but Gatekeeper finds its notarization online.
mkdir -p "$work/$name"
cp "$app/Contents/MacOS/squint" "$root/LICENSE" "$root/README.md" "$work/$name/"
tar -C "$work" -czf "$out/$name.tar.gz" "$name"

ls -la "$out"
