#!/bin/sh
# Installs the yetack daemon that the app sent, as a systemd user service, then says what
# else the daemon needs. Linux only. It never runs sudo.
#
#   ssh HOST sh -s -- --built FILE --sha256 SUM --version TEXT < contrib/install-daemon.sh
#   ssh HOST sh -s -- --check < contrib/install-daemon.sh
#
#   --built     the binary the app sent, which the script deletes once it is done
#   --sha256    the checksum FILE must have
#   --version   what `FILE --version` must print after "yetack "
#   --check     only says what the daemon needs, as the last lines of an install do
#
# It installs the binary as ~/.local/share/yetack/bin/yetack, writes
# ~/.config/systemd/user/yetack.service and starts it. Then it prints whether systemd keeps
# the user's services running after they log out ("linger yes"), and where the service finds
# claude ("claude PATH", or "claude missing").
#
# Exit status:
#   0  the daemon runs
#   1  failed before anything changed
#   4  failed during the install or the start

# The first line of the unit the install writes, which tells it from a unit the user wrote.
UNIT_MARK="which the desktop app installed"

say() {
    printf 'install-daemon: %s\n' "$*" 2>/dev/null || true
}

fail() {
    status=$1
    shift
    printf 'install-daemon: %s\n' "$*" >&2
    exit "$status"
}

die() {
    fail 1 "$@"
}

die_installing() {
    fail 4 "$@"
}

parse_arguments() {
    sent=
    sha256=
    version=
    check_only=
    while [ "$#" -gt 0 ]; do
        if [ "$1" = --check ]; then
            check_only=yes
            shift
            continue
        fi
        [ "$#" -ge 2 ] || die "$1 needs a value"
        case $1 in
            --built) sent=$2 ;;
            --sha256) sha256=$2 ;;
            --version) version=$2 ;;
            *) die "unknown option: $1" ;;
        esac
        shift 2
    done
    if [ -z "$check_only" ] && { [ -z "$sent" ] || [ -z "$sha256" ] || [ -z "$version" ]; }; then
        die "--built, --sha256 and --version are required"
    fi
}

# The binary the app sent must be the file it read, and this app's build.
check_sent() {
    say "checking $sent"
    [ -f "$sent" ] || die "$sent is missing"
    command -v sha256sum >/dev/null 2>&1 || die "cannot check the binary the app sent: this needs sha256sum"
    sum=$(sha256sum "$sent" | cut -d ' ' -f 1)
    [ "$sum" = "$sha256" ] || die "the binary on this machine is not the one the app sent: its checksum differs"
    chmod 700 "$sent" || die "cannot make $sent executable"
    said=$("$sent" --version 2>&1 | head -n 1)
    [ "$said" = "yetack $version" ] ||
        die "the binary the app sent says \"$said\", not \"yetack $version\""
}

# A command run over SSH may have no user session bus, which `systemctl --user` needs.
check_systemd() {
    command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1 ||
        die "this machine runs no systemd user session: start the daemon with contrib/start-daemon instead"
}

# A service of this name runs a daemon the user set up: the install must not replace its unit, and
# `enable --now` would leave its daemon running. A unit an earlier install wrote is the app's own.
check_no_service() {
    unit=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/yetack.service
    [ -L "$unit" ] || ! grep -q "$UNIT_MARK" "$unit" 2>/dev/null || return 0
    if [ -e "$unit" ] || [ -L "$unit" ] || systemctl --user cat yetack >/dev/null 2>&1; then
        die "a systemd user service yetack exists already: give the path of the daemon it runs, or remove the service first"
    fi
}

install_binary() {
    folder=$HOME/.local/share/yetack
    binary=$folder/bin/yetack
    mkdir -p "$folder/bin" && chmod 700 "$folder" || die_installing "cannot create $folder/bin"
    install -m 755 "$sent" "$binary.new" || die_installing "cannot write $binary.new"
    mv -f "$binary.new" "$binary" || die_installing "cannot write $binary"
    say "installed $binary"
}

# The PATH the service runs claude with: the unit's, as systemd expands it.
service_path() {
    printf '%s' "$HOME/.local/bin:$HOME/.cargo/bin:$HOME/.nix-profile/bin:/etc/profiles/per-user/$(id -un)/bin:/run/current-system/sw/bin:/usr/local/bin:/usr/bin:/bin"
}

# contrib/yetack.service, run from where this script installs the binary.
write_unit() {
    units=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
    mkdir -p "$units" || die_installing "cannot create $units"
    cat >"$units/yetack.service" <<'EOF' || die_installing "cannot write $units/yetack.service"
# The yetack daemon as a systemd user service, which the desktop app installed.

[Unit]
Description=Yetack daemon: Claude Code agents steered from the desktop app

[Service]
Type=exec
ExecStart=%h/.local/share/yetack/bin/yetack serve
Environment=PATH=%h/.local/bin:%h/.cargo/bin:%h/.nix-profile/bin:/etc/profiles/per-user/%u/bin:/run/current-system/sw/bin:/usr/local/bin:/usr/bin:/bin
# SIGTERM goes to the daemon alone: it stops with its agents still marked as running, so the next
# start resumes them. Their `claude` processes end with it.
KillMode=mixed
TimeoutStopSec=30
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
EOF
}

start_service() {
    say "starting the service"
    systemctl --user daemon-reload </dev/null || die_installing "systemctl could not reload the user units"
    systemctl --user enable --now yetack </dev/null ||
        die_installing "systemctl could not start yetack"
    tries=0
    until "$binary" list >/dev/null 2>&1; do
        tries=$((tries + 1))
        [ "$tries" -le 30 ] || die_installing "the daemon did not answer within 30 seconds"
        sleep 1
    done
    say "the daemon runs"
}

# Without linger, systemd stops the user's services, the daemon among them, when they log out.
report_needs() {
    linger=$(loginctl show-user "$(id -un)" -p Linger --value 2>/dev/null)
    say "linger ${linger:-unknown}"
    claude=$(PATH=$(service_path) command -v claude 2>/dev/null)
    say "claude ${claude:-missing}"
}

main() {
    # A lost connection must not stop the install halfway.
    trap '' HUP PIPE
    parse_arguments "$@"
    if [ -n "$check_only" ]; then
        report_needs
        return
    fi
    trap 'rm -f "$sent"' EXIT
    check_sent
    check_systemd
    check_no_service
    install_binary
    write_unit
    start_service
    report_needs
}

# The shell reads this script from a pipe as it runs: with everything in functions, it holds the
# whole script before it does anything, so a lost connection cannot cut the script short.
main "$@"
