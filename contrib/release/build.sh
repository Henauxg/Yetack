#!/bin/sh
# Builds the Linux release files inside the release container (contrib/release/Dockerfile), from
# the source tree in the current folder, and writes them to /out:
#   yetack-<version>-x86_64-linux   the daemon and command line, static (musl)
#   yetack_<version>_amd64.deb      the app, with the daemon beside it, which its updates send to
#                                   servers
#   yetack.service                  the systemd user service, for a server
#   SHA256SUMS                      with the Windows zip and app already in /out, if any
# YETACK_SOURCE_COMMIT is the commit the tree comes from, which the daemon tells the app and the
# app's "Update" builds on a server. OWNER is the uid:gid the files in /out get. YETACK_PACKAGE
# tells the app in the .deb that apt installed it, so it does not replace itself.
set -eu
: "${YETACK_SOURCE_COMMIT:?}" "${OWNER:?}"

version=$(cargo pkgid -p yetack | sed 's/.*[#@]//')
musl=x86_64-unknown-linux-musl

cargo build -q --release --locked --target "$musl" -p yetack
mkdir -p dist
cp "target/$musl/release/yetack" dist/yetack
strip dist/yetack
YETACK_LINUX_DAEMON=/usr/bin/yetack YETACK_PACKAGE=deb     cargo deb -q --locked -p cockpit_desktop --output /out/

cp dist/yetack "/out/yetack-$version-x86_64-linux"
cp contrib/yetack.service /out/
cd /out
sha256sum yetack* > SHA256SUMS
chown -R "$OWNER" /out
