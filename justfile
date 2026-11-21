# Inside `nix develop`, or from a plain terminal: nix develop -c just <recipe>
set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

# Lists the recipes.
default:
    @just --list

# Runs the app, which also starts the local background service when the config asks for it.
run:
    cargo run --release -p cockpit_desktop

# The Rust toolchain's own linker builds the static Linux daemon on any host, Windows included.
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER := "rust-lld"

# The static Linux daemon that `linux-daemon` builds, and the name the app looks for beside itself.
linux_daemon := "target/x86_64-unknown-linux-musl/release/yetack"
beside_the_app := "yetack-x86_64-linux"

# Needs the target: rustup target add x86_64-unknown-linux-musl. The app's "Update" sends the
# daemon to servers, which then build nothing.

# Builds the static Linux daemon beside the app's debug and release builds.
[unix]
linux-daemon:
    cargo build --release --locked --target x86_64-unknown-linux-musl -p yetack --bin yetack
    for profile in debug release; do mkdir -p "target/$profile" && cp {{linux_daemon}} "target/$profile/{{beside_the_app}}"; done

# Builds the static Linux daemon beside the app's debug and release builds.
[windows]
linux-daemon:
    cargo build --release --locked --target x86_64-unknown-linux-musl -p yetack --bin yetack
    foreach ($profile in 'debug', 'release') { New-Item -ItemType Directory -Force "target/$profile" | Out-Null; Copy-Item {{linux_daemon}} "target/$profile/{{beside_the_app}}" }

# Builds the daemon program and the app in release mode.
build:
    cargo build --release -p yetack -p cockpit_desktop

# Runs what CI runs, and shellcheck on the update script.
check: test clippy shellcheck

# Runs the workspace's tests.
test:
    cargo test -q --workspace

# Runs clippy as CI does.
clippy:
    cargo clippy -q --workspace --all-targets -- -D warnings

# Checks the shell scripts in contrib/.
[unix]
shellcheck:
    shellcheck -s sh contrib/update-daemon.sh contrib/install-daemon.sh contrib/release/build.sh contrib/release/install.sh contrib/readme-screenshot.sh

# Checks the shell scripts, on Linux only.
[windows]
shellcheck:
    @echo "shellcheck runs on Linux only"

# Formats the code. It needs the dev shell's nightly rustfmt.
fmt:
    cargo fmt

standard_repo := "https://github.com/Henauxg/rust-standard"

# The copies are replaced whole, so a change goes to rust-standard first.

# Copies the coding standard and the review skills from a rust-standard tag, branch or full commit hash, staged.
update-standard ref:
    git fetch -q {{ standard_repo }} '{{ ref }}'
    git rm -r -q -f --ignore-unmatch docs/standards/rust .claude/skills/review .claude/skills/review-standard .claude/skills/review-architecture
    git read-tree --prefix=docs/standards/rust/ -u FETCH_HEAD:standard
    git read-tree --prefix=.claude/skills/review/ -u FETCH_HEAD:skills/review
    git read-tree --prefix=.claude/skills/review-standard/ -u FETCH_HEAD:skills/review-standard
    git read-tree --prefix=.claude/skills/review-architecture/ -u FETCH_HEAD:skills/review-architecture
    git log -1 --format="Copied rust-standard %h (%s). Commit it." FETCH_HEAD

# The service's agents stop with it and resume at its next start. On a server, nothing starts it
# again by itself.

# Stops this machine's background service, so that the app's next start runs the fresh build.
[unix]
service-restart:
    #!/usr/bin/env sh
    set -eu
    lock=$(readlink -m "${XDG_STATE_HOME:-$HOME/.local/state}/yetack/serve.lock")
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

# The commit must be on GitHub. The binary's path defaults to ~/.local/bin/yetack.

# Updates a server's daemon to this checkout's commit.
[unix]
update-server host binary="":
    #!/usr/bin/env sh
    set -eu
    binary='{{ binary }}'
    if [ -z "$binary" ]; then
        binary="$(ssh '{{ host }}' 'echo "$HOME"')/.local/bin/yetack"
    fi
    exec ssh '{{ host }}' sh -s -- --commit "$(git rev-parse HEAD)" --binary "$binary" <contrib/update-daemon.sh

# The tree must be committed: the files are built from HEAD, in an Ubuntu 22.04 container.

# Builds the Linux release files and SHA256SUMS in dist/. On Windows, run it from Git Bash.
release:
    #!/usr/bin/env sh
    set -eu
    dist="$PWD/dist"
    # Docker Desktop takes a Windows path, and Git Bash would rewrite the container's paths.
    if command -v cygpath >/dev/null; then
        export MSYS_NO_PATHCONV=1
        dist=$(cygpath -m "$dist")
    fi
    if [ -n "$(git status --porcelain)" ]; then
        echo "commit or stash your changes first: the release builds HEAD" >&2
        exit 1
    fi
    docker build -q -t yetack-release contrib/release >/dev/null
    # This version's Windows zip and app stay in dist/ and go into SHA256SUMS. Any other file
    # would be published with the release, so the recipe refuses it.
    version=$(cargo pkgid --locked -p yetack | sed 's/.*[#@]//')
    : "${version:?could not read the version from Cargo.lock}"
    mkdir -p dist
    for file in dist/*; do
        [ -e "$file" ] || continue
        case "${file#dist/}" in
            "yetack-desktop-$version-windows-x64.zip" | "yetack-desktop-$version-windows-x64.exe") ;;
            yetack-*-x86_64-linux | yetack-*-aarch64-linux | yetack_*_amd64.deb | yetack-*.x86_64.rpm \
                | yetack-desktop-*-x86_64-linux.tar.gz | yetack.service | SHA256SUMS)
                rm -f "$file" ;;
            *)
                echo "$file is not a file of release $version: move it out of dist/ first" >&2
                exit 1 ;;
        esac
    done
    # Git for Windows writes CRLF line ends into an archive, as it does on checkout. The packages
    # need LF. tar -m dates every file now: with the commit's dates, cargo would reuse the cached
    # builds of a later commit.
    git -c core.autocrlf=false -c core.eol=lf archive --format=tar HEAD | docker run --rm -i \
        -e YETACK_SOURCE_COMMIT="$(git rev-parse HEAD)" -e OWNER="$(id -u):$(id -g)" \
        -v "$dist:/out" \
        -v yetack-release-registry:/opt/cargo/registry \
        -v yetack-release-target:/src/target \
        yetack-release sh -c 'tar -xm && exec sh contrib/release/build.sh'
    ls -l dist

# Needs Xvfb, openbox, wmctrl, xdotool and ImageMagick: see the script.

# Retakes the README's pictures, docs/images/<scene>.png, from the scenes of the demo's showcase.
[unix]
readme-screenshot:
    cargo build -q --release -p cockpit_desktop
    sh contrib/readme-screenshot.sh
