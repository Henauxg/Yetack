# Inside `nix develop`, or from a plain terminal: nix develop -c just <recipe>
set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

# Lists the recipes.
default:
    @just --list

# Runs the app, which also starts the local background service when the config asks for it.
run:
    cargo run --release -p cockpit_desktop

# Builds the daemon program and the app in release mode.
build:
    cargo build --release -p agents_cockpit -p cockpit_desktop

# Runs what CI runs, and shellcheck on the update script.
check: test clippy shellcheck

# Runs the workspace's tests.
test:
    cargo test -q --workspace

# Runs clippy as CI does.
clippy:
    cargo clippy -q --workspace --all-targets -- -D warnings

# Checks the update script.
[unix]
shellcheck:
    shellcheck -s sh contrib/update-daemon.sh

# Checks the update script, on Linux only.
[windows]
shellcheck:
    @echo "shellcheck runs on Linux only"

# Formats the code. It needs the dev shell's nightly rustfmt.
fmt:
    cargo fmt

# The service's agents stop with it and resume at its next start. On a server, nothing starts it
# again by itself.

# Stops this machine's background service, so that the app's next start runs the fresh build.
[unix]
service-restart:
    #!/usr/bin/env sh
    set -eu
    lock=$(readlink -m "${XDG_STATE_HOME:-$HOME/.local/state}/agents-cockpit/serve.lock")
    holder() {
        find /proc/[0-9]*/fd -maxdepth 1 -type l -printf '%l\t%p\n' 2>/dev/null |
            awk -F '\t' -v lock="$lock" '$1 == lock { split($2, parts, "/"); print parts[3]; exit }'
    }
    pid=$(holder)
    if [ -z "$pid" ]; then
        echo "no background service runs here"
        exit 0
    fi
    kill -TERM "$pid"
    tries=0
    while [ -n "$(holder)" ]; do
        tries=$((tries + 1))
        if [ "$tries" -gt 30 ]; then
            echo "the service, pid $pid, did not stop within 30 seconds" >&2
            exit 1
        fi
        sleep 1
    done
    echo "the service stopped; the app's next start runs the new build"

# The commit must be on GitHub. The binary's path defaults to ~/.local/bin/agents-cockpit.

# Updates a server's daemon to this checkout's commit.
[unix]
update-server host binary="":
    #!/usr/bin/env sh
    set -eu
    binary='{{ binary }}'
    if [ -z "$binary" ]; then
        binary="$(ssh '{{ host }}' 'echo "$HOME"')/.local/bin/agents-cockpit"
    fi
    exec ssh '{{ host }}' sh -s -- --commit "$(git rev-parse HEAD)" --binary "$binary" <contrib/update-daemon.sh
