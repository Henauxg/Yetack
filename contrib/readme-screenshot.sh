#!/bin/sh
# Takes the README's pictures, docs/images/<scene>.png, one per scene of the demo's showcase
# (--demo --demo-showcase <scene>). It runs the app in a virtual display of 2560 by 1440, wide
# enough for a reply's comment to show beside its passage.
#
#   just readme-screenshot
#
# Needs a release build and Xvfb, openbox, wmctrl, xdotool and ImageMagick (magick). On Nix:
#   nix shell nixpkgs#openbox nixpkgs#wmctrl nixpkgs#xdotool nixpkgs#imagemagick
set -eu

app=${CARGO_TARGET_DIR:-target}/release/yetack-desktop
scenes="machines projects"
[ -x "$app" ] || { echo "build $app first: cargo build --release -p cockpit_desktop" >&2; exit 1; }

# Other sessions may take screenshots on this machine, so use the first free display from 160.
display=160
while [ -e "/tmp/.X$display-lock" ]; do display=$((display + 1)); done
work=$(mktemp -d)
pids=""
stop() {
    # shellcheck disable=SC2086
    [ -z "$pids" ] || kill $pids 2>/dev/null || true
    rm -rf "$work"
}
trap stop EXIT
# dash, which is sh on Debian and Ubuntu, skips the EXIT trap when a signal ends the script.
trap 'exit 130' INT
trap 'exit 143' TERM HUP

Xvfb ":$display" -screen 0 2560x1440x24 >/dev/null 2>&1 &
pids="$!"
sleep 1.5
kill -0 "$pids" 2>/dev/null || { echo "Xvfb did not start on :$display" >&2; exit 1; }
export DISPLAY=":$display"
openbox >/dev/null 2>&1 &
pids="$pids $!"
sleep 1

for scene in $scenes; do
    # The app's config, state, data and cache folders point at the scratch folder, so the app
    # leaves no files behind.
    XDG_CONFIG_HOME="$work" XDG_STATE_HOME="$work" XDG_DATA_HOME="$work" XDG_CACHE_HOME="$work" \
        "$app" --demo --demo-showcase "$scene" >"$work/$scene.log" 2>&1 &
    app_pid=$!
    pids="$pids $app_pid"
    sleep 15
    window=$(wmctrl -l | awk 'NR == 1 { print $1 }')
    [ -n "$window" ] || { echo "the app opened no window: $work/$scene.log" >&2; cat "$work/$scene.log" >&2; exit 1; }
    wmctrl -i -r "$window" -b add,maximized_vert,maximized_horz
    # The pointer rests on the window's bottom edge, where it hovers nothing.
    xdotool mousemove 1300 1439
    sleep 2
    import -window root "$work/$scene.png"
    wmctrl -i -c "$window"
    sleep 2
    kill "$app_pid" 2>/dev/null || true
    # The window manager's title bar is 19 pixels high.
    out="docs/images/$scene.png"
    magick "$work/$scene.png" -crop 2560x1421+0+19 +repage -strip -define png:compression-level=9 "$out"
    echo "wrote $out"
done
