#!/usr/bin/env bash
# The Linux AppImage, built from a release binary on x86_64 Linux:
#   packaging/linux/appimage.sh 4.4.0 target/release/ciranda ../dist
# It writes ciranda-<version>-linux-x64.AppImage and its SHA-256 beside it, one line in
# sha256sum's format, and ciranda-<version>-linux-x64.AppImage.zsync: the AppImage carries the
# update information that points at the newest GitHub release's .zsync, so AppImageUpdate (and the
# launchers built on it) fetches only the blocks that changed. That takes zsyncmake on PATH (the
# zsync package).
# linuxdeploy copies in the shared libraries the binary needs that a desktop can't be counted on
# to have, and leaves out the ones that must come from the system: those on its excludelist
# (glibc, ALSA, GL and Vulkan drivers, X11 and libwayland-client, fontconfig and FreeType) and
# those in host_libs below. So the build machine's glibc is the oldest one it runs on.
# LICENSE, THIRD_PARTY_NOTICES.md and THIRD_PARTY_LICENSES.txt (the crates' licences, written by
# cargo-about: see about.toml) go in usr/share/doc/ciranda. The last is read from
# target/THIRD_PARTY_LICENSES.txt, or from $CIRANDA_LICENSES.
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
licenses=${CIRANDA_LICENSES:-$here/../../target/THIRD_PARTY_LICENSES.txt}
[ -f "$binary" ] || { echo "No binary at $binary" >&2; exit 1; }
[ -f "$licenses" ] || { echo "No crate licences at $licenses: run cargo about generate (see about.toml)" >&2; exit 1; }
command -v file >/dev/null || { echo "The AppImage tool needs the file command: apt-get install file" >&2; exit 1; }
command -v zsyncmake >/dev/null || { echo "The .zsync file needs zsyncmake: apt-get install zsync" >&2; exit 1; }
binary=$(cd "$(dirname "$binary")" && pwd)/$(basename "$binary")
mkdir -p "$out_dir"
out_dir=$(cd "$out_dir" && pwd)
name="ciranda-$version-linux-x64.AppImage"
# Read by AppImageUpdate from inside the AppImage: the newest release's .zsync whose name matches.
update_information="gh-releases-zsync|Brunovncs|ciranda|latest|ciranda-*-linux-x64.AppImage.zsync"

# Dated releases rather than "continuous", which is rebuilt in place, and the hashes catch a
# download that isn't the one this was tested with; bump each URL and its hash together. The
# runtime is the small program at the front of every AppImage that mounts the rest; left alone,
# the AppImage tool would fetch whatever "continuous" holds that day.
linuxdeploy_url=https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage
linuxdeploy_sha256=c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d
runtime_url=https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64
runtime_sha256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d

# Libraries that work with the rest of the desktop, so they must be the system's own, matching its
# daemons, configuration and data, and are never copied in:
host_libs=(
  # GLib, which build.rs links for libwebrtc's screen capture portal. One bundled from Ubuntu
  # 22.04 would load the system's GIO modules (gvfs, dconf), built against a newer GLib, and fail
  # on their missing symbols. What only GLib needs stays out with it.
  'libglib-2.0.so*' 'libgio-2.0.so*' 'libgobject-2.0.so*' 'libgmodule-2.0.so*' 'libgthread-2.0.so*'
  'libffi.so*' 'libpcre.so*' 'libpcre2-8.so*' 'libmount.so*' 'libblkid.so*' 'libselinux.so*'
  # Clients of the system's D-Bus, PulseAudio (or PipeWire's stand-in for it) and udev, which
  # speak those daemons' protocols and read their configuration and hardware database.
  'libdbus-1.so*' 'libpulse.so*' 'libpulse-simple.so*' 'libpulsecommon-*.so' 'libudev.so*' 'libsystemd.so*'
  # xkbcommon reads the system's keyboard layouts, from where its own build was told to look.
  'libxkbcommon.so*' 'libxkbcommon-x11.so*'
  # The Vulkan loader finds the system's GPU drivers; the rest of Wayland goes with
  # libwayland-client, which linuxdeploy already leaves out.
  'libvulkan.so*' 'libwayland-*.so*'
)
exclude_args=()
for lib in "${host_libs[@]}"; do
  exclude_args+=(--exclude-library "$lib")
done

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

# The desktop file and the AppStream metadata go by the reverse-DNS id: the AppImage tool finds
# the metadata under the desktop file's name and checks one against the other (ciranda.metainfo.xml).
id=io.github.brunovncs.ciranda
appdir="$work/AppDir"
install -Dm755 "$binary" "$appdir/usr/bin/ciranda"
install -Dm644 "$here/ciranda.desktop" "$appdir/usr/share/applications/$id.desktop"
install -Dm644 "$here/ciranda.metainfo.xml" "$appdir/usr/share/metainfo/$id.appdata.xml"
install -Dm644 "$assets/icon.png" "$appdir/usr/share/icons/hicolor/256x256/apps/ciranda.png"
install -Dm644 "$here/../../../LICENSE" "$appdir/usr/share/doc/ciranda/LICENSE"
install -Dm644 "$here/../../THIRD_PARTY_NOTICES.md" "$appdir/usr/share/doc/ciranda/THIRD_PARTY_NOTICES.md"
install -Dm644 "$licenses" "$appdir/usr/share/doc/ciranda/THIRD_PARTY_LICENSES.txt"
ln -s usr/bin/ciranda "$appdir/AppRun"
ln -s usr/share/icons/hicolor/256x256/apps/ciranda.png "$appdir/.DirIcon"

# linuxdeploy builds the AppImage in the current directory; OUTPUT is the AppImage tool's name
# for the file, LDAI_OUTPUT the plugin's, and the plugin passes the latter on, as it does
# LDAI_UPDATE_INFORMATION, with which the AppImage tool also writes <name>.zsync. Extracting
# itself to run leaves a copy behind in TMPDIR, so that points inside the directory removed on exit.
cd "$work"
ARCH=x86_64 LDAI_OUTPUT="$name" OUTPUT="$name" LINUXDEPLOY_OUTPUT_VERSION="$version" \
  LDAI_UPDATE_INFORMATION="$update_information" \
  LDAI_RUNTIME_FILE="$runtime" APPIMAGE_EXTRACT_AND_RUN=1 TMPDIR="$work" \
  "$linuxdeploy" --appimage-extract-and-run \
    --appdir "$appdir" \
    --executable "$appdir/usr/bin/ciranda" \
    --desktop-file "$appdir/usr/share/applications/$id.desktop" \
    --icon-file "$appdir/usr/share/icons/hicolor/256x256/apps/ciranda.png" \
    "${exclude_args[@]}" \
    --output appimage

[ -f "$work/$name.zsync" ] || { echo "The AppImage tool wrote no $name.zsync" >&2; exit 1; }
mv "$work/$name" "$out_dir/$name"
mv "$work/$name.zsync" "$out_dir/$name.zsync"
(cd "$out_dir" && sha256sum "$name" > "$name.sha256")
echo "$out_dir/$name"
