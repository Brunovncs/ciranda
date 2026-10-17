#!/usr/bin/env bash
# The Linux AppImage, built from a release binary on x86_64 Linux:
#   packaging/linux/appimage.sh 4.4.0 target/release/ciranda ../dist
# It writes ciranda-<version>-linux-x64.AppImage and its SHA-256 beside it, one line in
# sha256sum's format. linuxdeploy copies in the shared libraries the binary needs that a desktop
# can't be counted on to have (libssl and libcrypto from native-tls, when built on Ubuntu 22.04)
# and leaves out the ones that must come from the system, per its excludelist: glibc, ALSA, GL
# and Vulkan, X11 and Wayland. So the build machine's glibc is the oldest one it runs on.
# linuxdeploy runs with --appimage-extract-and-run, so neither it nor the AppImage tool it starts
# needs FUSE, which containers and CI runners often lack.
set -euo pipefail

if [ $# -ne 3 ]; then
  echo "Usage: $0 <version> <path to the ciranda binary> <output dir>" >&2
  exit 2
fi
version=$1
binary=$2
out_dir=$3

here=$(cd "$(dirname "$0")" && pwd)
assets="$here/../../assets"
[ -f "$binary" ] || { echo "No binary at $binary" >&2; exit 1; }
command -v file >/dev/null || { echo "The AppImage tool needs the file command: apt-get install file" >&2; exit 1; }
binary=$(cd "$(dirname "$binary")" && pwd)/$(basename "$binary")
mkdir -p "$out_dir"
out_dir=$(cd "$out_dir" && pwd)
name="ciranda-$version-linux-x64.AppImage"

# Dated releases rather than "continuous", which is rebuilt in place, and the hashes catch a
# download that isn't the one this was tested with; bump each URL and its hash together. The
# runtime is the small program at the front of every AppImage that mounts the rest; left alone,
# the AppImage tool would fetch whatever "continuous" holds that day.
linuxdeploy_url=https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage
linuxdeploy_sha256=c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d
runtime_url=https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64
runtime_sha256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fetch() {
  curl -fsSL --retry 3 -o "$3" "$1"
  echo "$2  $3" | sha256sum -c --quiet -
}
linuxdeploy="$work/linuxdeploy-x86_64.AppImage"
runtime="$work/runtime-x86_64"
fetch "$linuxdeploy_url" "$linuxdeploy_sha256" "$linuxdeploy"
fetch "$runtime_url" "$runtime_sha256" "$runtime"
chmod +x "$linuxdeploy"

appdir="$work/AppDir"
install -Dm755 "$binary" "$appdir/usr/bin/ciranda"
install -Dm644 "$here/ciranda.desktop" "$appdir/usr/share/applications/ciranda.desktop"
install -Dm644 "$assets/icon.png" "$appdir/usr/share/icons/hicolor/256x256/apps/ciranda.png"
ln -s usr/bin/ciranda "$appdir/AppRun"
ln -s usr/share/icons/hicolor/256x256/apps/ciranda.png "$appdir/.DirIcon"

# linuxdeploy builds the AppImage in the current directory; OUTPUT is the AppImage tool's name
# for the file, LDAI_OUTPUT the plugin's, and the plugin passes the latter on. Extracting itself
# to run leaves a copy behind in TMPDIR, so that points inside the directory removed on exit.
cd "$work"
ARCH=x86_64 LDAI_OUTPUT="$name" OUTPUT="$name" LINUXDEPLOY_OUTPUT_VERSION="$version" \
  LDAI_RUNTIME_FILE="$runtime" APPIMAGE_EXTRACT_AND_RUN=1 TMPDIR="$work" \
  "$linuxdeploy" --appimage-extract-and-run \
    --appdir "$appdir" \
    --executable "$appdir/usr/bin/ciranda" \
    --desktop-file "$appdir/usr/share/applications/ciranda.desktop" \
    --icon-file "$appdir/usr/share/icons/hicolor/256x256/apps/ciranda.png" \
    --output appimage

mv "$work/$name" "$out_dir/$name"
(cd "$out_dir" && sha256sum "$name" > "$name.sha256")
echo "$out_dir/$name"
