#!/usr/bin/env bash
# Builds Ciranda.app around a built ciranda binary and packs it into a disk image, from any
# directory:
#   packaging/macos/bundle.sh <version> <ciranda binary> <out dir> [arch label]
# It writes <out dir>/ciranda-<version>-macos-<arch label>.dmg (the label is universal unless
# given) and its SHA-256 beside it, in the same form as the Windows release files. The disk image
# holds the app and a link to /Applications, so installing is dragging one onto the other.
#
# The app is signed ad-hoc, with no identity, and not notarized: there is no Apple Developer ID.
# Apple Silicon refuses to run unsigned code, and an ad-hoc signature also seals Info.plist and
# the icon. It is signed without the hardened runtime, which only notarization asks for; with it,
# the microphone and camera would also need entitlements, and without it they need none, so there
# is no entitlements file. Gatekeeper still blocks an app downloaded unnotarized: on first launch
# people right-click Ciranda and choose Open (on macOS 15 and later, open it once, then System
# Settings > Privacy & Security > Open Anyway), or run
#   xattr -dr com.apple.quarantine /Applications/Ciranda.app
# An ad-hoc signature changes with every build, so macOS asks again for the microphone, the camera
# and the screen after each update.
set -euo pipefail

usage() {
  echo "usage: $0 <version> <ciranda binary> <out dir> [arch label]" >&2
  exit 2
}
[ $# -ge 3 ] && [ $# -le 4 ] || usage

version=$1
binary=$2
out_dir=$3
arch=${4:-universal}

# CFBundleVersion and CFBundleShortVersionString take only numbers and dots.
[[ $version =~ ^[0-9]+(\.[0-9]+){0,2}$ ]] || { echo "Not a version macOS takes: $version" >&2; exit 1; }
[[ $arch =~ ^[A-Za-z0-9_-]+$ ]] || { echo "Not an arch label: $arch" >&2; exit 1; }
[ -f "$binary" ] || { echo "No binary at $binary" >&2; exit 1; }

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
plist_template="$script_dir/Info.plist"
icon_png="$script_dir/../../assets/icon.png"
[ -f "$plist_template" ] || { echo "No Info.plist at $plist_template" >&2; exit 1; }
[ -f "$icon_png" ] || { echo "No icon at $icon_png" >&2; exit 1; }

mkdir -p "$out_dir"
out_dir=$(cd "$out_dir" && pwd)

work=$(mktemp -d "${TMPDIR:-/tmp}/ciranda-bundle.XXXXXX")
trap 'rm -rf "$work"' EXIT

app="$work/stage/Ciranda.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

cp "$binary" "$app/Contents/MacOS/ciranda"
chmod 755 "$app/Contents/MacOS/ciranda"

sed "s/@VERSION@/$version/g" "$plist_template" > "$app/Contents/Info.plist"
if grep -q '@VERSION@' "$app/Contents/Info.plist"; then
  echo "Info.plist still has a placeholder" >&2
  exit 1
fi
plutil -lint "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"

# The icon is 256 pixels, so the 512 and 1024 sizes are scaled up and a little soft; a bigger
# assets/icon.png would sharpen them with no change here.
iconset="$work/ciranda.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  sips -s format png -z "$size" "$size" "$icon_png" --out "$iconset/icon_${size}x${size}.png" > /dev/null
  double=$((size * 2))
  sips -s format png -z "$double" "$double" "$icon_png" --out "$iconset/icon_${size}x${size}@2x.png" > /dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/ciranda.icns"

# Extended attributes picked up on the way (quarantine, Finder info) make codesign refuse the bundle.
xattr -cr "$app"
codesign --force --deep --sign - "$app"
codesign --verify --deep --strict --verbose=2 "$app"
lipo -info "$app/Contents/MacOS/ciranda"

ln -s /Applications "$work/stage/Applications"

name="ciranda-$version-macos-$arch.dmg"
dmg="$out_dir/$name"
# hdiutil on CI runners now and then fails with "Resource busy" while something still looks at
# the new volume; another try a few seconds later goes through.
for attempt in 1 2 3; do
  if hdiutil create -volname Ciranda -srcfolder "$work/stage" -ov -format UDZO "$dmg"; then
    break
  fi
  if [ "$attempt" -eq 3 ]; then
    echo "hdiutil failed three times" >&2
    exit 1
  fi
  sleep 5
done

(cd "$out_dir" && shasum -a 256 "$name") > "$dmg.sha256"
cat "$dmg.sha256"
