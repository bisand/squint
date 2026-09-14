#!/usr/bin/env bash
# Packages a built squint for Linux: a tarball, a .deb, an .rpm and an
# AppImage, all from the one binary.
#
#   packaging/linux/package.sh <version> <arch> <binary> <out-dir>
#
# <arch> is the machine's name for itself: x86_64 or aarch64.
set -euo pipefail

version="$1"
arch="$2"
binary="$3"
out="$(mkdir -p "$4" && cd "$4" && pwd)"
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

case "$arch" in
  x86_64) deb_arch=amd64 ;;
  aarch64) deb_arch=arm64 ;;
  *) echo "unknown arch: $arch" >&2; exit 1 ;;
esac
# A pre-release's hyphen sorts after the release in dpkg and rpm; a tilde
# sorts before it, which is what 1.0.0-rc1 means.
pkg_version="${version//-/\~}"
name="squint-$version-linux-$arch"

# One tree laid out as /usr, which every package below is made from.
stage="$work/stage"
install -Dm755 "$binary" "$stage/usr/bin/squint"
# The release profile keeps symbols for profiling; a package has no use for
# them, and they are most of its weight.
strip "$stage/usr/bin/squint"
install -Dm644 "$here/squint.desktop" "$stage/usr/share/applications/squint.desktop"
install -Dm644 "$root/packaging/icons/squint-256.png" "$stage/usr/share/icons/hicolor/256x256/apps/squint.png"
install -Dm644 "$root/packaging/icons/squint.png" "$stage/usr/share/icons/hicolor/1024x1024/apps/squint.png"
install -Dm644 "$root/LICENSE" "$stage/usr/share/licenses/squint/LICENSE"
install -Dm644 "$root/README.md" "$stage/usr/share/doc/squint/README.md"

# Tarball: the tree, to be unpacked over /usr/local or anywhere else.
mkdir -p "$work/$name"
cp -R "$stage/usr/." "$work/$name/"
tar -C "$work" -czf "$out/$name.tar.gz" "$name"

# Debian and Ubuntu. winit and wgpu open the display and GPU libraries at run
# time rather than linking them, so they are recommendations, not depends.
deb="$work/deb"
cp -R "$stage" "$deb"
mkdir -p "$deb/DEBIAN" "$deb/usr/share/doc/squint"
installed_size="$(du -sk "$deb/usr" | cut -f1)"
cat > "$deb/DEBIAN/control" <<EOF
Package: squint
Version: $pkg_version
Architecture: $deb_arch
Maintainer: André Biseth <andre@biseth.net>
Installed-Size: $installed_size
Depends: libc6, libxkbcommon0
Recommends: libwayland-client0, libx11-6, libxcursor1, libxrandr2, libxi6, libxkbcommon-x11-0, libvulkan1 | libgl1, xdg-desktop-portal
Section: editors
Priority: optional
Homepage: https://github.com/bisand/squint
Description: text editor for files too big for text editors
 squint opens a file of any size instantly, holds almost none of it in
 memory, shows it with syntax highlighting, and lets you tweak it and save.
EOF
cp "$root/LICENSE" "$deb/usr/share/doc/squint/copyright"
dpkg-deb --root-owner-group --build "$deb" "$out/squint_${pkg_version}_${deb_arch}.deb"

# Fedora, RHEL, openSUSE: an rpm around the same tree.
rpmtop="$work/rpm"
mkdir -p "$rpmtop"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}
cat > "$rpmtop/SPECS/squint.spec" <<EOF
Name:           squint
Version:        $pkg_version
Release:        1
Summary:        Text editor for files too big for text editors
License:        MIT
URL:            https://github.com/bisand/squint
Requires:       libxkbcommon
Recommends:     libwayland-client, libX11, libXcursor, libXrandr, libXi, libxkbcommon-x11, vulkan-loader
AutoReqProv:    no

%global debug_package %{nil}
%global __strip /bin/true

%description
squint opens a file of any size instantly, holds almost none of it in
memory, shows it with syntax highlighting, and lets you tweak it and save.

%install
cp -a $stage/. %{buildroot}/

%files
/usr/bin/squint
/usr/share/applications/squint.desktop
/usr/share/icons/hicolor/256x256/apps/squint.png
/usr/share/icons/hicolor/1024x1024/apps/squint.png
%license /usr/share/licenses/squint/LICENSE
%doc /usr/share/doc/squint/README.md
EOF
rpmbuild --define "_topdir $rpmtop" --target "$arch" -bb "$rpmtop/SPECS/squint.spec"
cp "$rpmtop"/RPMS/"$arch"/*.rpm "$out/"

# AppImage: runs on any distribution with a glibc as new as the builder's.
appdir="$work/squint.AppDir"
cp -R "$stage" "$appdir"
cp "$here/squint.desktop" "$appdir/squint.desktop"
cp "$root/packaging/icons/squint-256.png" "$appdir/squint.png"
ln -s squint.png "$appdir/.DirIcon"
cat > "$appdir/AppRun" <<'EOF'
#!/bin/sh
here="$(dirname "$(readlink -f "$0")")"
exec "$here/usr/bin/squint" "$@"
EOF
chmod +x "$appdir/AppRun"
tool="$work/appimagetool"
curl -fsSL -o "$tool" "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$arch.AppImage"
chmod +x "$tool"
ARCH="$arch" APPIMAGE_EXTRACT_AND_RUN=1 "$tool" --no-appstream "$appdir" "$out/$name.AppImage"

ls -la "$out"
