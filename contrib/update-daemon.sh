#!/bin/sh
# Updates this machine's agents-cockpit daemon to one commit and restarts it. Linux only.
#
#   ssh HOST sh -s -- --commit HASH --binary PATH [--source-dir DIR] [--even-if-busy] < contrib/update-daemon.sh
#   ssh HOST sh -s -- --cancel < contrib/update-daemon.sh
#
#   --commit        a commit or ref that the source checkout can fetch from origin
#   --binary        the absolute path of the agents-cockpit binary to replace
#   --source-dir    a git checkout of the repository, by default ~/agents-cockpit
#   --even-if-busy  restarts even while agents are busy or may hold a scheduled job: the restart
#                   ends their turns, which the new daemon resumes, and loses their jobs
#   --cancel        makes the running update stop before it installs: its build runs on
#
# It builds outside the checkout, which stays as it is. It restarts the daemon with
# $XDG_CONFIG_HOME/agents-cockpit/start-daemon when that file exists (a script that starts the
# daemon in the background and returns), and otherwise with the agents-cockpit systemd user
# service. Without --even-if-busy, it refuses while an agent works, waits for an answer, runs
# background tasks or may hold a scheduled job.
# Once started it finishes even when the connection is lost: build.log, in its folder under
# $XDG_CACHE_HOME/agents-cockpit/update, keeps the build's output.
#
# Exit status:
#   0  the daemon runs the new version
#   1  failed before anything changed
#   3  refused: agents are busy or may hold a scheduled job
#   4  failed during the install or the restart: the daemon may be down
#   5  the new daemon did not start, and the previous version runs again
#   6  refused: another update is running
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
    even_if_busy=
    while [ "$#" -gt 0 ]; do
        if [ "$1" = --even-if-busy ]; then
            even_if_busy=yes
            shift
            continue
        fi
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

# The start time of a process that runs, empty for a dead one. A zombie counts as dead.
start_time() {
    sed 's/.*) //' "/proc/$1/stat" 2>/dev/null | awk '$1 != "Z" { print $20 }'
}

# The start time tells the lock's owner from a later process that got its pid.
lock_owner_runs() {
    read -r owner_pid owner_start 2>/dev/null <"$lock" || return 1
    [ -n "$owner_start" ] && [ "$(start_time "$owner_pid")" = "$owner_start" ]
}

# Two updates at once would build in the same folder and restart the daemon twice. A killed update
# leaves its lock, so a dead owner's lock counts as free. flock makes looking at the lock and
# replacing it one step, so that only one update takes a dead owner's lock.
take_lock() {
    mkdir -p "$work_dir" || die "cannot create $work_dir"
    # Written first and moved in, so that nobody sees a lock without its owner.
    claim=$lock.$$
    printf '%s %s\n' "$$" "$(start_time "$$")" >"$claim" || die "cannot write $claim"
    (
        flock -n 9 || exit 6
        ! lock_owner_runs || exit 6
        mv -f "$claim" "$lock"
    ) 9>"$lock.guard"
    taken=$?
    rm -f "$claim"
    [ "$taken" != 6 ] || fail 6 "another update is running"
    [ "$taken" = 0 ] || die "cannot take $lock"
    trap 'rm -f "$lock"' EXIT
    trap 'exit 1' INT TERM
    rm -rf "$work_dir/cancel"
}

# A cancel creates the name `cancel` as a file and the install as a folder. Creating a name is
# atomic, so one of them wins and a cancel cannot slip in after the update's last look.
request_cancel() {
    lock_owner_runs || die "no update is running"
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
    command -v flock >/dev/null 2>&1 || die "cannot take the update lock: this needs flock"
    if ! lists_locks ||
        ! find "/proc/$$/fd" -maxdepth 1 -lname '*' >/dev/null 2>&1 ||
        [ -z "$(ps -o etimes= -p "$$" 2>/dev/null)" ]; then
        die "cannot tell whether a daemon runs: this needs a /proc/locks that lists locks, a stat with -c, a find with -lname and a ps with -o etimes"
    fi
}

# Whether /proc/locks lists a lock taken on a scratch file. Some sandboxed kernels list none. It
# hides a lock whose process ended, so flock holds this one while awk reads.
lists_locks() {
    probe=$(mktemp) || return 1
    inode=$(stat -L -c %i "$probe" 2>/dev/null)
    listed=$([ -n "$inode" ] && flock -n "$probe" awk -v inode="$inode" "$lockers_program" /proc/locks)
    rm -f "$probe"
    [ -n "$listed" ]
}

# The pids that took a lock on the file of inode $1, from /proc/locks, which gives the file as
# device:inode. A waiter's line has `->` first. On Btrfs, stat gives another device than
# /proc/locks, so only the inode is compared.
lockers_program='$2 == "FLOCK" && $6 ~ (":" inode "$") { print $5 }'
lockers() {
    awk -v inode="$1" "$lockers_program" /proc/locks
}

# The path of the state folder's lock file as `find -lname` matches it: that takes a shell pattern,
# and this path must match as it is written.
lock_pattern() {
    readlink -f "$state_dir/serve.lock" | sed 's/[][*?\\]/\\&/g'
}

# The daemon of this state folder is the process that holds the lock on its lock file, whatever
# binary it runs. Others may have the file open, such as its child between fork and exec. Since
# only the inode is compared, the process must also have the file open.
daemon_pid() {
    inode=$(stat -L -c %i "$state_dir/serve.lock" 2>/dev/null) || return 0
    for pid in $(lockers "$inode"); do
        if has_lock_file_open "$pid"; then
            echo "$pid"
            return 0
        fi
    done
}

has_lock_file_open() {
    [ -n "$(find "/proc/$1/fd" -maxdepth 1 -lname "$(lock_pattern)" 2>/dev/null)" ]
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
    if ! listing=$("$binary" list 2>&1); then
        # After a failed restart the old daemon runs on, and only the previous binary can read it.
        listing_error=$listing
        if [ ! -f "$binary.previous" ] || ! listing=$("$binary.previous" list 2>&1); then
            die "cannot read the agents from the daemon: $listing_error"
        fi
    fi
    [ -z "$even_if_busy" ] || return 0
    # A resting agent runs no turn: it is idle, or it waits only for its account's sign-in, with
    # nothing to answer. The columns are found by their headers: the listing of an older binary
    # has no TASKS, and without WAIT every agent that needs you counts as busy.
    # shellcheck disable=SC2016
    resting='NR == 1 {
            for (i = 1; i <= NF; i++) {
                if ($i == "WAIT") wait_column = i
                if ($i == "TASKS") tasks_column = i
            }
            next
        }
        { resting = $2 == "idle" || ($2 == "needs_you" && wait_column && $wait_column == 0) }'
    # A resting agent whose background tasks still run is busy too.
    busy=$(printf '%s\n' "$listing" | awk "$resting"'
        $2 != "exited" && (!resting || (tasks_column && $tasks_column > 0))')
    [ -z "$busy" ] || refuse "not restarting the daemon, these agents are busy:
$busy"
    # A scheduled job lives in its claude process, so none outlasts the daemon's last start. `ps`
    # counts whole seconds, so the start is taken a little early.
    started_at=$((($(date +%s) - $(ps -o etimes= -p "$daemon") - 2) * 1000))
    holding=
    for id in $(printf '%s\n' "$listing" | awk "$resting"' resting { print $1 }'); do
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
    # A folder left by another checkout, moved or cloned again since, cannot check this commit out.
    if [ ! -d "$tree" ] || ! git -C "$tree" checkout --quiet --detach --force "$sha" </dev/null 2>/dev/null; then
        rm -rf "$tree"
        git -C "$source_dir" worktree prune
        git -C "$source_dir" worktree add --quiet --detach "$tree" "$sha" </dev/null ||
            die "cannot check out $sha in $tree"
    fi
    say "building $sha"
    # The dev shell unsets CARGO_TARGET_DIR, so the folder goes on the command line.
    target_dir=$work_dir/target
    set -- cargo build --release --locked -p agents_cockpit \
        --target-dir "$target_dir" --jobs "${CARGO_BUILD_JOBS:-4}"
    if [ -f "$tree/flake.nix" ] && command -v nix >/dev/null 2>&1; then
        set -- nix --extra-experimental-features 'nix-command flakes' develop --command "$@"
    fi
    # cargo stops when it cannot write its output. It writes to tee, which outlives the connection:
    # with SIGPIPE ignored, a failed write only makes it stop copying to its standard output.
    # Only this subshell enters the checkout, so that a daemon started later does not run in it.
    status_file=$work_dir/build.status
    rm -f "$status_file"
    { cd "$tree" && nice -n 19 "$@" </dev/null 2>&1; echo "$?" >"$status_file"; } | tee "$work_dir/build.log"
    [ "$(cat "$status_file" 2>/dev/null)" = 0 ] ||
        die "the build of $sha failed; the daemon was not touched"
    built=$target_dir/release/agents-cockpit
    [ -x "$built" ] || die "the build of $sha made no $built"
}

# Signals the process that holds the lock, and waits until none does. A pid names the daemon by its
# start time, read once the pid is found and checked to hold the lock again: right before the
# signal, the start time must be the same, so a process that got the pid since gets no signal.
stop_daemon() {
    tries=0
    signalled=
    while daemon=$(daemon_pid) && [ -n "$daemon" ]; do
        started=$(start_time "$daemon")
        if [ -n "$started" ] && [ "$daemon $started" != "$signalled" ] &&
            [ "$(daemon_pid)" = "$daemon" ] && [ "$(start_time "$daemon")" = "$started" ]; then
            if kill -TERM "$daemon" 2>/dev/null; then
                signalled="$daemon $started"
            elif [ "$(start_time "$daemon")" = "$started" ]; then
                die_installing "cannot stop the daemon, pid $daemon"
            fi
        fi
        tries=$((tries + 1))
        [ "$tries" -le 30 ] || die_installing "the daemon, pid $daemon, did not stop within 30 seconds"
        sleep 1
    done
}

# On failure `why` says what went wrong.
restart_daemon() {
    case $restart in
        systemd)
            systemctl --user restart agents-cockpit </dev/null || {
                why="systemctl could not restart the daemon"
                return 1
            }
            ;;
        start_file)
            stop_daemon
            # A background process left holding this output would keep the SSH session open. The
            # daemon and its agents would keep the signals ignored here.
            (trap - HUP INT TERM PIPE && exec sh "$start_file") </dev/null >>"$work_dir/start-daemon.log" 2>&1 || {
                why="$start_file failed, see $work_dir/start-daemon.log"
                return 1
            }
            ;;
    esac
}

# On failure `why` says what went wrong.
wait_for_daemon() {
    tries=0
    until "$binary" list >/dev/null 2>&1; do
        tries=$((tries + 1))
        if [ "$tries" -gt 30 ]; then
            why="the daemon did not answer within 30 seconds"
            return 1
        fi
        sleep 1
    done
}

# `list` answers for any daemon of this state folder, such as one the restart did not replace.
check_new_binary_runs() {
    running=$(readlink "/proc/$(daemon_pid)/exe" 2>/dev/null)
    [ "$running" = "$(readlink -f "$binary")" ] ||
        die_installing "the daemon that answers runs ${running:-an unknown binary}, not $binary"
}

# The new daemon never answered: the binary before it goes back in place and runs again.
roll_back() {
    failure="the new version did not start: $why"
    if [ ! -f "$binary.previous" ] || cmp -s "$binary.previous" "$built"; then
        die_installing "$failure"
    fi
    say "going back to the previous version"
    { install -m 755 "$binary.previous" "$binary.new" && mv -f "$binary.new" "$binary"; } ||
        die_installing "$failure; cannot put $binary.previous back"
    if ! restart_daemon || ! wait_for_daemon; then
        die_installing "$failure; the previous version did not come back either: $why"
    fi
    # By content: a daemon the restart left running has a binary that was replaced since.
    cmp -s "/proc/$(daemon_pid)/exe" "$binary" ||
        die_installing "$failure; the daemon that answers does not run the previous version"
    fail 5 "$failure; the previous version runs again"
}

install_and_restart() {
    # A connection lost from here on must not leave the machine without a daemon.
    trap '' HUP INT TERM PIPE
    mkdir -p "$(dirname "$binary")" || die_installing "cannot create the folder of $binary"
    # A retry after a failed restart must keep the last binary that worked.
    if [ -f "$binary" ] && ! cmp -s "$binary" "$built"; then
        cp -f "$binary" "$binary.previous" || die_installing "cannot save $binary"
    fi
    install -m 755 "$built" "$binary.new" || die_installing "cannot write $binary.new"
    mv -f "$binary.new" "$binary" || die_installing "cannot replace $binary"
    say "restarting the daemon"
    rm -f "$work_dir/start-daemon.log"
    if restart_daemon && wait_for_daemon; then
        check_new_binary_runs
        say "the daemon runs $sha"
    else
        roll_back
    fi
}

main() {
    # A lost connection must not stop the update: a client that goes away closes the output pipes,
    # and may send SIGHUP.
    trap '' HUP PIPE
    work_dir=${XDG_CACHE_HOME:-$HOME/.cache}/agents-cockpit/update
    lock=$work_dir/lock.pid
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
