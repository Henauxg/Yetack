#!/bin/sh
# Installs the yetack daemon that the app sent, as a systemd user service, or started on its own
# where systemd runs no user session. Then it says what else the daemon needs. Linux only. It
# never runs sudo.
#
#   ssh HOST sh -s -- --built FILE --sha256 SUM --version TEXT < contrib/install-daemon.sh
#   ssh HOST sh -s -- --check [--binary BINARY] < contrib/install-daemon.sh
#   ssh HOST sh -s -- --add-boot-start [--binary BINARY] < contrib/install-daemon.sh
#   ssh HOST sh -s -- --start BINARY < contrib/install-daemon.sh
#
#   --built           the binary the app sent, which the script deletes once it is done
#   --sha256          the checksum FILE must have
#   --version         what `FILE --version` must print after "yetack "
#   --check           only says what the daemon needs, as the last lines of an install do
#   --add-boot-start  adds the line that starts the installed daemon when the machine starts to
#                     the user's crontab, then says what the daemon needs. The app runs it only
#                     once the user asked for it.
#   --start           starts the daemon BINARY names, where none runs: its systemd user service
#                     when it has one, else on its own as an install without systemd does
#   --binary          with --check or --add-boot-start, the daemon BINARY names, in place of the
#                     installed one, for the start command and the crontab line
#
# It installs the binary as ~/.local/share/yetack/bin/yetack. With a systemd user session, it
# writes ~/.config/systemd/user/yetack.service and starts it, then says whether systemd keeps the
# user's services running after they log out ("linger yes"). Without one, it starts the daemon
# outside the SSH session, so that it keeps running once the session ends, and says whether the
# user's crontab starts it again when the machine starts ("boot yes", "boot no", or "boot
# unavailable" without crontab), the command that starts it ("start COMMAND"), and with crontab the
# line --add-boot-start puts there ("cron LINE") and whether a cron daemon runs to read it ("crond
# yes", "crond no", or "crond unknown" when it cannot see the system's processes). In both cases it
# says which of the two starts the daemon ("service systemd" or "service none"), and where the daemon finds claude ("claude PATH",
# or "claude missing").
#
# Exit status:
#   0  the daemon runs
#   1  failed before anything changed
#   4  failed during the install or the start

# The first line of the unit the install writes, which tells it from a unit the user wrote.
UNIT_MARK="which the desktop app installed"

# The end of the crontab line the install adds, which tells it from the user's own lines.
CRON_MARK="# yetack: the daemon, which the desktop app starts"

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
    start_binary=
    action=install
    while [ "$#" -gt 0 ]; do
        case $1 in
            --check)
                action=check
                shift
                continue ;;
            --add-boot-start)
                action=boot
                shift
                continue ;;
        esac
        [ "$#" -ge 2 ] || die "$1 needs a value"
        case $1 in
            --start)
                action=start
                start_binary=$2 ;;
            --binary) start_binary=$2 ;;
            --built) sent=$2 ;;
            --sha256) sha256=$2 ;;
            --version) version=$2 ;;
            *) die "unknown option: $1" ;;
        esac
        shift 2
    done
    if [ "$action" = install ] && { [ -z "$sent" ] || [ -z "$sha256" ] || [ -z "$version" ]; }; then
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
has_systemd() {
    command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1
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

binary=$HOME/.local/share/yetack/bin/yetack

install_binary() {
    folder=$HOME/.local/share/yetack
    if ! mkdir -p "$folder/bin" || ! chmod 700 "$folder"; then
        die_installing "cannot create $folder/bin"
    fi
    install -m 755 "$sent" "$binary.new" || die_installing "cannot write $binary.new"
    mv -f "$binary.new" "$binary" || die_installing "cannot write $binary"
    say "installed $binary"
}

# The PATH the daemon runs claude with: the unit's, as systemd expands it, also for a daemon started
# without systemd.
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

# Waits for the daemon to answer. Given the process the script started and its log, a daemon that
# ends at once fails at once, with its last word.
wait_for_daemon() {
    started=${1:-}
    tries=0
    until "$binary" list >/dev/null 2>&1; do
        if [ -n "$started" ] && ! kill -0 "$started" 2>/dev/null; then
            die_installing "the daemon stopped as it started: $(tail -n 1 "$2" 2>/dev/null)"
        fi
        tries=$((tries + 1))
        [ "$tries" -le 30 ] || die_installing "the daemon did not answer within 30 seconds"
        sleep 1
    done
    say "the daemon runs"
}

start_service() {
    say "starting the service"
    systemctl --user daemon-reload </dev/null || die_installing "systemctl could not reload the user units"
    systemctl --user enable --now yetack </dev/null ||
        die_installing "systemctl could not start yetack"
    wait_for_daemon
}

# Where the daemon started without systemd writes what it prints.
daemon_log() {
    printf '%s' "${XDG_STATE_HOME:-$HOME/.local/state}/yetack/daemon.log"
}

# Starts the daemon in a session of its own, which the end of this SSH session does not stop. A
# daemon that runs already stays: a second one would find the state folder locked.
start_detached() {
    say "starting the daemon"
    if "$binary" list >/dev/null 2>&1; then
        say "the daemon runs"
        return
    fi
    log=$(daemon_log)
    mkdir -p "$(dirname "$log")" || die_installing "cannot create the folder of $log"
    path=$(service_path)
    launcher=$(command -v setsid || command -v nohup) ||
        die_installing "cannot start the daemon apart from this session: this needs setsid or nohup"
    # The script ignores HUP and PIPE for itself: the daemon and its agents take them as a
    # service's do.
    (
        trap - HUP PIPE
        exec env PATH="$path" "$launcher" "$binary" serve
    ) </dev/null >>"$log" 2>&1 &
    wait_for_daemon "$!" "$log"
}

# The command that starts the daemon, with the PATH a start by hand gives it, as a crontab line or
# the user's own init system runs it.
start_command() {
    printf 'PATH=%s %s serve >>%s 2>&1' "$(shell_word "$(service_path)")" "$(shell_word "$binary")" \
        "$(shell_word "$(daemon_log)")"
}

# WORD as one word of a shell command: in single quotes when it holds more than letters, digits
# and the signs of a plain path.
shell_word() {
    case $1 in
        '' | *[!A-Za-z0-9_./:@+=-]*)
            printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")" ;;
        *) printf '%s' "$1" ;;
    esac
}

# The crontab line that starts the daemon when the machine starts.
cron_line() {
    printf '@reboot %s %s' "$(start_command)" "$CRON_MARK"
}

# Whether the user's crontab starts the daemon when the machine starts: yes, no, or unavailable
# when the machine has no crontab.
boot_state() {
    if ! command -v crontab >/dev/null 2>&1; then
        printf unavailable
    elif crontab -l 2>/dev/null | grep -qF "$CRON_MARK"; then
        printf yes
    else
        printf no
    fi
}

# Puts the line in the user's crontab, in place of one an earlier run added, and keeps the
# others.
add_boot_start() {
    command -v crontab >/dev/null 2>&1 || die "this machine has no crontab: start $binary serve when it starts with its own init system"
    [ -x "$binary" ] || die "$binary is not installed"
    line=$(cron_line)
    case $line in
        *%*) die "cron reads a % as the end of the command, and the line holds one: add it by hand" ;;
    esac
    # Read first: a crontab that wrote as it read would lose the user's lines. A crontab that
    # cannot be read stays, since a rewrite would lose them too.
    if ! tab=$(crontab -l 2>/dev/null); then
        why=$(crontab -l 2>&1 >/dev/null)
        case $why in
            *"no crontab"* | *"No such file"*) tab= ;;
            *) die "cannot read your crontab, so it stays as it is: ${why:-crontab -l failed}" ;;
        esac
    fi
    others=$(printf '%s\n' "$tab" | grep -vF "$CRON_MARK")
    {
        [ -z "$others" ] || printf '%s\n' "$others"
        printf '%s\n' "$line"
    } | crontab - || die_installing "crontab refused the new line"
    say "added the start at boot to the crontab"
}

# The daemon the app names, as the probe finds it: a bare command on the folders the user's own
# programs are in, which a command run over SSH leaves off the PATH.
resolve_binary() {
    case $start_binary in
        /*) binary=$start_binary ;;
        *) binary=$(PATH=$PATH:$HOME/.local/share/yetack/bin:$HOME/.local/bin && command -v "$start_binary") ||
            die "$start_binary is not installed" ;;
    esac
    [ -x "$binary" ] || die "$binary is not installed"
}

# Starts the daemon where none runs: the user's service when it has one, else on its own.
start_installed() {
    resolve_binary
    if "$binary" list >/dev/null 2>&1; then
        say "the daemon runs"
        return
    fi
    if has_systemd && systemctl --user cat yetack >/dev/null 2>&1; then
        say "starting the service"
        systemctl --user start yetack </dev/null || die_installing "systemctl could not start yetack"
        wait_for_daemon
    else
        start_detached
    fi
}

# Whether a cron daemon runs, which a crontab line needs to start anything: yes, no, or unknown
# without pgrep, or where /proc hides other users' processes (hidepid). OpenRC, for one, often does
# not start cron by default.
crond_state() {
    if ! command -v pgrep >/dev/null 2>&1 || ! [ -r /proc/1/stat ]; then
        printf unknown
        return
    fi
    for name in crond cron cronie fcron dcron; do
        if pgrep -x "$name" >/dev/null 2>&1; then
            printf yes
            return
        fi
    done
    printf no
}

# Without linger, systemd stops the user's services, the daemon among them, when they log out. A
# daemon started without systemd keeps running then, until the machine restarts.
report_needs() {
    if has_systemd; then
        say "service systemd"
        linger=$(loginctl show-user "$(id -un)" -p Linger --value 2>/dev/null)
        say "linger ${linger:-unknown}"
    else
        say "service none"
        boot=$(boot_state)
        say "boot $boot"
        say "start $(start_command)"
        [ "$boot" = unavailable ] || say "cron $(cron_line)"
        [ "$boot" = unavailable ] || say "crond $(crond_state)"
    fi
    claude=$(PATH=$(service_path) command -v claude 2>/dev/null)
    say "claude ${claude:-missing}"
}

main() {
    # A lost connection must not stop the install halfway.
    trap '' HUP PIPE
    parse_arguments "$@"
    case $action in
        check | boot) [ -z "$start_binary" ] || resolve_binary ;;
    esac
    case $action in
        check)
            report_needs
            return ;;
        boot)
            add_boot_start
            report_needs
            return ;;
        start)
            start_installed
            return ;;
    esac
    trap 'rm -f "$sent"' EXIT
    check_sent
    if has_systemd; then
        check_no_service
        install_binary
        write_unit
        start_service
    else
        install_binary
        start_detached
    fi
    report_needs
}

# The shell reads this script from a pipe as it runs: with everything in functions, it holds the
# whole script before it does anything, so a lost connection cannot cut the script short.
main "$@"
