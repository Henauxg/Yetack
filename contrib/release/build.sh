#!/bin/sh
# Builds the Linux release files inside the release container (contrib/release/Dockerfile), from
# the source tree in the current folder, and writes them to /out:
#   agents-cockpit-<version>-x86_64-linux   the daemon and command line, static (musl)
#   agents-cockpit_<version>_amd64.deb      the app, with the daemon beside it
#   agents-cockpit.service                  the systemd user service, for a server
#   SHA256SUMS                              with a Windows zip already in /out, if any
# AGENTS_COCKPIT_SOURCE_COMMIT is the commit the tree comes from, which the daemon tells the app and the
# app's "Update" builds on a server. OWNER is the uid:gid the files in /out get.
set -eu
: "${AGENTS_COCKPIT_SOURCE_COMMIT:?}" "${OWNER:?}"

version=$(cargo pkgid -p agents_cockpit | sed 's/.*[#@]//')
musl=x86_64-unknown-linux-musl

cargo build -q --release --locked --target "$musl" -p agents_cockpit
mkdir -p dist
cp "target/$musl/release/agents-cockpit" dist/agents-cockpit
strip dist/agents-cockpit
cargo deb -q --locked -p cockpit_desktop --output /out/

cp dist/agents-cockpit "/out/agents-cockpit-$version-x86_64-linux"
cp contrib/agents-cockpit.service /out/
cd /out
sha256sum agents-cockpit* > SHA256SUMS
chown -R "$OWNER" /out
