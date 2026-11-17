#!/bin/sh
# Builds the Linux release files inside the release container (contrib/release/Dockerfile), from
# the source tree in the current folder, and writes them to /out:
#   yetack-<version>-x86_64-linux                 the daemon and command line, static (musl)
#   yetack-<version>-aarch64-linux                the same for ARM servers
#   yetack_<version>_amd64.deb                    the app for Ubuntu and Debian, with the daemon
#                                                 beside it, which its updates send to servers
#   yetack-<version>-1.x86_64.rpm                 the same files and libxdo, for Fedora and openSUSE
#   yetack-desktop-<version>-x86_64-linux.tar.gz  the app for any other Linux, with the daemon and
#                                                 an install.sh that copies them to ~/.local
#   yetack.service                                the systemd user service, for a server
#   SHA256SUMS                                    with the Windows zip and app already in /out, if any
# YETACK_SOURCE_COMMIT is the commit the tree comes from, which the daemon tells the app and the
# app's "Update" builds on a server. OWNER is the uid:gid the files in /out get. YETACK_PACKAGE
# tells each app how it was installed, which decides what its update card says.
set -eu
: "${YETACK_SOURCE_COMMIT:?}" "${OWNER:?}"

version=$(cargo pkgid -p yetack | sed 's/.*[#@]//')
musl=x86_64-unknown-linux-musl

cargo build -q --release --locked --target "$musl" -p yetack
mkdir -p dist
cp "target/$musl/release/yetack" dist/yetack
strip dist/yetack

# The daemon for ARM servers. The Rust toolchain's own linker builds it, and strips it: the
# container's strip knows only x86_64.
arm=aarch64-unknown-linux-musl
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C strip=symbols" \
    cargo build -q --release --locked --target "$arm" -p yetack
cp "target/$arm/release/yetack" dist/yetack-aarch64-linux

# The .deb and the .rpm send this one to an ARM server, and /usr/bin/yetack to the others.
arm_daemon=/usr/lib/yetack/yetack-aarch64-linux
YETACK_LINUX_DAEMON=/usr/bin/yetack YETACK_LINUX_DAEMON_AARCH64=$arm_daemon YETACK_PACKAGE=deb \
    cargo deb -q --locked -p cockpit_desktop --output /out/

# Recent distros no longer have Ubuntu 22.04's libxdo.so.3 (Fedora 45, openSUSE Tumbleweed and
# Arch have libxdo.so.4), so the .rpm and the tarball carry it. The app finds it through its
# RUNPATH, before the system's own.
libxdo=/usr/lib/x86_64-linux-gnu/libxdo.so.3

# The .rpm holds the .deb's files and libxdo, in /usr/lib/yetack. Each package builds the app
# again, for its own YETACK_PACKAGE. The copies in dist/ are stripped and patched, not cargo's
# own files.
YETACK_LINUX_DAEMON=/usr/bin/yetack YETACK_LINUX_DAEMON_AARCH64=$arm_daemon YETACK_PACKAGE=rpm \
    cargo build -q --release --locked -p cockpit_desktop
rm -rf dist/rpm
mkdir -p dist/rpm
cp target/release/yetack-desktop dist/rpm/
strip dist/rpm/yetack-desktop
# shellcheck disable=SC2016 # $ORIGIN is for the loader, not for the shell.
patchelf --set-rpath '$ORIGIN/../lib/yetack' dist/rpm/yetack-desktop
cp -L "$libxdo" dist/rpm/
cargo generate-rpm -p crates/cockpit_desktop -o /out/

# The tarball's app finds the daemon beside itself, as the Windows zip's does. It finds libxdo in
# lib/ beside it in the tarball, and in lib/yetack/ once install.sh has copied it.
YETACK_PACKAGE=tarball cargo build -q --release --locked -p cockpit_desktop
folder=yetack-desktop-$version-x86_64-linux
rm -rf "dist/$folder"
mkdir -p "dist/$folder"
cp target/release/yetack-desktop "dist/$folder/"
strip "dist/$folder/yetack-desktop"
# shellcheck disable=SC2016 # $ORIGIN is for the loader, not for the shell.
patchelf --set-rpath '$ORIGIN/lib:$ORIGIN/../lib/yetack' "dist/$folder/yetack-desktop"
mkdir -p "dist/$folder/lib"
cp -L "$libxdo" "dist/$folder/lib/"
cp dist/yetack "dist/$folder/yetack-x86_64-linux"
cp dist/yetack-aarch64-linux "dist/$folder/"
cp contrib/release/install.sh contrib/yetack-desktop.desktop crates/cockpit_desktop/assets/yetack.svg \
    crates/cockpit_desktop/assets/yetack-256.png LICENSE "dist/$folder/"
chmod 755 "dist/$folder/install.sh"
tar -C dist --owner=0 --group=0 --sort=name -czf "/out/$folder.tar.gz" "$folder"

cp dist/yetack "/out/yetack-$version-x86_64-linux"
cp dist/yetack-aarch64-linux "/out/yetack-$version-aarch64-linux"
cp contrib/yetack.service /out/
cd /out
sha256sum yetack* > SHA256SUMS
chown -R "$OWNER" /out
