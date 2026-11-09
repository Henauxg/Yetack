#!/bin/sh
# Updates this machine's agents-cockpit daemon to one commit and restarts it. Linux only.
#
#   ssh HOST sh -s -- --commit HASH --binary PATH [--source-dir DIR] < contrib/update-daemon.sh
#   ssh HOST sh -s -- --cancel < contrib/update-daemon.sh
#
#   --commit      a commit or ref that the source checkout can fetch from origin
#   --binary      the absolute path of the agents-cockpit binary to replace
#   --source-dir  a git checkout of the repository, by default ~/agents-cockpit
#   --cancel      makes the running update stop before it installs: its build runs on
#
# It builds outside the checkout, which stays as it is. It restarts the daemon with
# $XDG_CONFIG_HOME/agents-cockpit/start-daemon when that file exists (a script that starts the
# daemon in the background and returns), and otherwise with the agents-cockpit systemd user
# service. It refuses while an agent works, waits for an answer or may hold a scheduled job.
# Once started it finishes even when the connection is lost: build.log, in its folder under
# $XDG_CACHE_HOME/agents-cockpit/update, keeps the build's output.
#
# Exit status:
#   0  the daemon runs the new version
#   1  failed before anything changed
#   3  refused: agents are busy or may hold a scheduled job
#   4  failed during the install or the restart
# With --cancel: 0 when the update will stop before it installs, 1 when none runs or it is too late.

say() {
    printf 'update-daemon: %s\n' "$*" 2>/dev/null || true
}

# Prints a message and ends the script with the status given first.
fail() {
    status=$1
    shift
    printf 'update-daemon: %s\n' "$*" >&2
    exit "$status"
}

die() {
    fail 1 "$@"
}

refuse() {
    fail 3 "$@"
}

die_installing() {
    fail 4 "$@"
}

parse_arguments() {
    commit=
    binary=
    source_dir=$HOME/agents-cockpit
    while [ "$#" -gt 0 ]; do
        [ "$#" -ge 2 ] || die "$1 needs a value"
        case $1 in
            --commit) commit=$2 ;;
            --binary) binary=$2 ;;
            --source-dir) source_dir=$2 ;;
            *) die "unknown option: $1" ;;
        esac
        shift 2
    done
    [ -n "$commit" ] || die "--commit is required"
    case $commit in
        -*) die "not a commit: $commit" ;;
    esac
    case $binary in
        /*) ;;
        *) die "--binary must be an absolute path" ;;
    esac
}

# Two updates at once would build in the same folder and restart the daemon twice.
take_lock() {
    mkdir -p "$work_dir" || die "cannot create $work_dir"
    mkdir "$work_dir/lock" 2>/dev/null ||
        die "another update is running; if none is, remove $work_dir/lock"
    trap 'rmdir "$work_dir/lock" 2>/dev/null' EXIT
    trap 'exit 1' INT TERM
    rm -rf "$work_dir/cancel"
}

# A cancel creates the name `cancel` as a file and the install as a folder. Creating a name is
# atomic, so one of them wins and a cancel cannot slip in after the update's last look.
request_cancel() {
    [ -d "$work_dir/lock" ] || die "no update is running"
    if (set -C && : >"$work_dir/cancel") 2>/dev/null || [ -f "$work_dir/cancel" ]; then
        say "the update stops before it installs, once its build ends"
    elif [ -d "$work_dir/cancel" ]; then
        die "too late to cancel: the update is installing"
    else
        die "cannot write $work_dir/cancel"
    fi
}

choose_restart() {
    if [ -f "$start_file" ]; then
        restart=start_file
    elif command -v systemctl >/dev/null 2>&1 &&
        { systemctl --user is-active --quiet agents-cockpit 2>/dev/null ||
            systemctl --user is-enabled --quiet agents-cockpit 2>/dev/null; }; then
        restart=systemd
    else
        die "cannot restart the daemon: no $start_file and no agents-cockpit systemd user service"
    fi
}

# Without these tools no daemon and no scheduled job would show, and a busy daemon would restart.
check_tools() {
    if ! find "/proc/$$/fd" -maxdepth 1 -lname '*' >/dev/null 2>&1 ||
        [ -z "$(ps -o etimes= -p "$$" 2>/dev/null)" ]; then
        die "cannot tell whether a daemon runs: this needs a find with -lname and a ps with -o etimes"
    fi
}

# The daemon of this state folder is the process holding its lock file, whatever binary it runs.
daemon_pid() {
    # `find -lname` takes a shell pattern, and this path must match as it is written.
    lock_pattern=$(readlink -f "$state_dir/serve.lock" | sed 's/[][*?\\]/\\&/g')
    find /proc/[0-9]*/fd -maxdepth 1 -lname "$lock_pattern" 2>/dev/null |
        sed -n '1s|^/proc/\([0-9]*\)/.*|\1|p'
}

# Prints how many times Claude called a tool in a raw log since a time in milliseconds. A raw line
# holds Claude's JSON as a string, so its quotes are escaped. Usage figures name tools too, so the
# pattern takes the whole start of a tool_use block.
count_calls() {
    awk -v since="$2" -F '[:,]' '$2 + 0 >= since && index($0, "\"direction\":\"from_claude\"")' "$1" |
        grep -o -E '\\"type\\":\\"tool_use\\",\\"id\\":\\"[^\\"]*\\",\\"name\\":\\"'"$3"'\\"' | wc -l
}

check_agents() {
    daemon=$(daemon_pid)
    [ -n "$daemon" ] || return 0
    listing=$("$binary" list 2>&1) || die "cannot read the agents from the daemon: $listing"
    busy=$(printf '%s\n' "$listing" | awk 'NR > 1 && $2 != "idle" && $2 != "exited"')
    [ -z "$busy" ] || refuse "not restarting the daemon, these agents are busy:
$busy"
    # A scheduled job lives in its claude process, so none outlasts the daemon's last start. `ps`
    # counts whole seconds, so the start is taken a little early.
    started_at=$((($(date +%s) - $(ps -o etimes= -p "$daemon") - 2) * 1000))
    holding=
    for id in $(printf '%s\n' "$listing" | awk 'NR > 1 && $2 == "idle" { print $1 }'); do
        log=$state_dir/raw/$id.jsonl
        [ -f "$log" ] || continue
        created=$(count_calls "$log" "$started_at" CronCreate)
        deleted=$(count_calls "$log" "$started_at" CronDelete)
        if [ "$created" -gt "$deleted" ]; then
            holding="$holding
$(printf '%s\n' "$listing" | awk -v id="$id" '$1 == id')"
        fi
    done
    [ -z "$holding" ] || refuse "not restarting the daemon, a restart would lose the scheduled jobs these agents may hold. Delete the jobs or stop the agents:$holding"
}

build() {
    export GIT_TERMINAL_PROMPT=0
    git -C "$source_dir" fetch --quiet origin </dev/null || die "cannot fetch in $source_dir"
    sha=$(git -C "$source_dir" rev-parse --verify --quiet "$commit^{commit}") ||
        die "commit $commit is not on origin: push it first"
    tree=$work_dir/source
    git -C "$source_dir" worktree prune
    if [ -d "$tree" ]; then
        git -C "$tree" checkout --quiet --detach --force "$sha" </dev/null ||
            die "cannot check out $sha in $tree"
    else
        git -C "$source_dir" worktree add --quiet --detach "$tree" "$sha" </dev/null ||
            die "cannot check out $sha in $tree"
    fi
    say "building $sha"
    cd "$tree" || die "cannot enter $tree"
    # The dev shell unsets CARGO_TARGET_DIR, so the folder goes on the command line.
    target_dir=$work_dir/target
    set -- cargo build --release --locked -p agents_cockpit \
        --target-dir "$target_dir" --jobs "${CARGO_BUILD_JOBS:-4}"
    if [ -f flake.nix ] && command -v nix >/dev/null 2>&1; then
        set -- nix --extra-experimental-features 'nix-command flakes' develop --command "$@"
    fi
    # cargo stops when it cannot write its output. It writes to tee, which outlives the connection:
    # with SIGPIPE ignored, a failed write only makes it stop copying to its standard output.
    status_file=$work_dir/build.status
    rm -f "$status_file"
    { nice -n 19 "$@" </dev/null 2>&1; echo "$?" >"$status_file"; } | tee "$work_dir/build.log"
    [ "$(cat "$status_file" 2>/dev/null)" = 0 ] ||
        die "the build of $sha failed; the daemon was not touched"
    built=$target_dir/release/agents-cockpit
    [ -x "$built" ] || die "the build of $sha made no $built"
}

stop_daemon() {
    daemon=$(daemon_pid)
    [ -n "$daemon" ] || return 0
    kill -TERM "$daemon" || die_installing "cannot stop the daemon, pid $daemon"
    tries=0
    while [ -n "$(daemon_pid)" ]; do
        tries=$((tries + 1))
        [ "$tries" -le 30 ] || die_installing "the daemon, pid $daemon, did not stop within 30 seconds"
        sleep 1
    done
}

restart_daemon() {
    case $restart in
        systemd)
            systemctl --user restart agents-cockpit </dev/null ||
                die_installing "systemctl could not restart the daemon"
            ;;
        start_file)
            stop_daemon
            # A background process left holding this output would keep the SSH session open.
            sh "$start_file" </dev/null >"$work_dir/start-daemon.log" 2>&1 ||
                die_installing "$start_file failed, see $work_dir/start-daemon.log"
            ;;
    esac
}

wait_for_daemon() {
    tries=0
    until "$binary" list >/dev/null 2>&1; do
        tries=$((tries + 1))
        [ "$tries" -le 30 ] ||
            die_installing "the new daemon did not answer within 30 seconds; the previous binary is $binary.previous"
        sleep 1
    done
}

# `list` answers for any daemon of this state folder, such as one the restart did not replace.
check_new_binary_runs() {
    running=$(readlink "/proc/$(daemon_pid)/exe" 2>/dev/null)
    [ "$running" = "$(readlink -f "$binary")" ] ||
        die_installing "the daemon that answers runs ${running:-an unknown binary}, not $binary"
}

install_and_restart() {
    # A connection lost from here on must not leave the machine without a daemon.
    trap '' HUP INT TERM PIPE
    mkdir -p "$(dirname "$binary")" || die_installing "cannot create the folder of $binary"
    [ ! -f "$binary" ] || cp -f "$binary" "$binary.previous" || die_installing "cannot save $binary"
    install -m 755 "$built" "$binary.new" || die_installing "cannot write $binary.new"
    mv -f "$binary.new" "$binary" || die_installing "cannot replace $binary"
    say "restarting the daemon"
    restart_daemon
    wait_for_daemon
    check_new_binary_runs
    say "the daemon runs $sha"
}

main() {
    # A lost connection must not stop the update: a client that goes away closes the output pipes,
    # and may send SIGHUP.
    trap '' HUP PIPE
    work_dir=${XDG_CACHE_HOME:-$HOME/.cache}/agents-cockpit/update
    if [ "$*" = --cancel ]; then
        request_cancel
        return
    fi
    parse_arguments "$@"
    # A command run over SSH loads no shell profile.
    PATH=$PATH:$HOME/.local/bin:$HOME/.cargo/bin:$HOME/.nix-profile/bin:/nix/var/nix/profiles/default/bin:/run/current-system/sw/bin
    PATH=$PATH:/etc/profiles/per-user/$(id -un)/bin
    state_dir=${XDG_STATE_HOME:-$HOME/.local/state}/agents-cockpit
    start_file=${XDG_CONFIG_HOME:-$HOME/.config}/agents-cockpit/start-daemon
    check_tools
    take_lock
    choose_restart
    check_agents
    build
    # The build takes minutes: an agent may have started working meanwhile.
    check_agents
    mkdir "$work_dir/cancel" 2>/dev/null || die "the update was cancelled; the daemon was not touched"
    install_and_restart
}

# The shell reads this script from a pipe as it runs: with everything in functions, it holds the
# whole script before it does anything, so a lost connection cannot cut the script short.
main "$@"
