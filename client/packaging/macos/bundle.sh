#!/usr/bin/env bash
# Builds Ciranda.app around a built ciranda binary and packs it into a disk image, from any
# directory:
#   packaging/macos/bundle.sh <version> <ciranda binary> <out dir> [arch label]
# It writes <out dir>/ciranda-<version>-macos-<arch label>.dmg (the label is universal unless
# given) and its SHA-256 beside it, in the same form as the Windows release files. The disk image
# holds the app and a link to /Applications, so installing is dragging one onto the other.
#
# LICENSE, THIRD_PARTY_NOTICES.md and THIRD_PARTY_LICENSES.txt (the crates' licences, written by
# cargo-about: see about.toml) go in Contents/Resources. The last is read from
# target/THIRD_PARTY_LICENSES.txt, or from $CIRANDA_LICENSES.
#
# The app is not notarized: there is no Apple Developer ID. It is signed, which Apple silicon needs
# to run anything at all and which seals Info.plist, the icon and the licences, by one of:
# - CIRANDA_SIGN_IDENTITY, a certificate's SHA-1 or name in a keychain on the search list.
#   release.yml imports the self-signed "Ciranda Self-Signed Code Signing" there from the
#   MACOS_SIGN_P12 secret. The signature's designated requirement then names that certificate,
#   the same in every release, so macOS keeps the microphone, camera and screen permissions
#   across updates.
# - Otherwise ad-hoc, with no identity (forks, pull requests, a build at home). That requirement
#   is the build's own hash, so after each update macOS asks for all three again.
# Either way with the hardened runtime and ciranda.entitlements: the hardened runtime refuses the
# microphone and the camera, without asking the person, to a program that doesn't claim them, and
# that file claims those two and nothing more. libwebrtc is linked in and generates no code at run
# time, GPUI's Metal shaders are compiled by the system's own service, and no library but the
# system's is loaded, so JIT, unsigned executable memory and disabled library validation would
# only widen what injected code could do. (An old-style virtual camera, a CoreMediaIO plug-in
# loaded into each app, is refused for that reason; camera extensions run on their own and work.)
# No secure timestamp: Apple's service is for certificates Apple issued, and nothing checks one
# without notarization.
#
# Gatekeeper still blocks an app downloaded unnotarized: on first launch people right-click
# Ciranda and choose Open (on macOS 15 and later, open it once, then System Settings > Privacy &
# Security > Open Anyway), or run
#   xattr -dr com.apple.quarantine /Applications/Ciranda.app
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
entitlements="$script_dir/ciranda.entitlements"
icon_png="$script_dir/../../assets/icon.png"
licenses=${CIRANDA_LICENSES:-$script_dir/../../target/THIRD_PARTY_LICENSES.txt}
identity=${CIRANDA_SIGN_IDENTITY:--}
[ -f "$plist_template" ] || { echo "No Info.plist at $plist_template" >&2; exit 1; }
[ -f "$entitlements" ] || { echo "No entitlements at $entitlements" >&2; exit 1; }
[ -f "$icon_png" ] || { echo "No icon at $icon_png" >&2; exit 1; }
[ -f "$licenses" ] || { echo "No crate licences at $licenses: run cargo about generate (see about.toml)" >&2; exit 1; }

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

cp "$script_dir/../../../LICENSE" "$app/Contents/Resources/LICENSE"
cp "$script_dir/../../THIRD_PARTY_NOTICES.md" "$app/Contents/Resources/THIRD_PARTY_NOTICES.md"
cp "$licenses" "$app/Contents/Resources/THIRD_PARTY_LICENSES.txt"

# Extended attributes picked up on the way (quarantine, Finder info) make codesign refuse the bundle.
xattr -cr "$app"
sign() {
  codesign --force --options runtime --timestamp=none --sign "$identity" "$@"
}
# Inside out, and not with --deep, which would hand the app's entitlements to everything nested:
# frameworks, plug-ins and helpers first (there are none today; the program is one file), then the
# app, which signs its executable with it.
while IFS= read -r -d '' nested; do
  sign "$nested"
done < <(find "$app/Contents" -mindepth 2 \( -name '*.dylib' -o -name '*.framework' -o -name '*.bundle' -o -name '*.app' -o -name '*.xpc' \) -prune -print0)
sign --entitlements "$entitlements" "$app"
codesign --verify --deep --strict --verbose=2 "$app"
# What macOS will hold the permissions to, and what the program claims.
codesign --display --requirements - "$app"
codesign --display --entitlements - "$app"
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
