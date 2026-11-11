#!/bin/sh
# Takes the README's picture, docs/images/app.png, from the demo's showcase (--demo
# --demo-showcase) in a virtual display of 2560 by 1440: wide enough for a reply's comment to
# show beside its passage. It drags the sidebar wider so that titles show whole.
#
#   just readme-screenshot
#
# Needs a release build and Xvfb, openbox, wmctrl, xdotool and ImageMagick (magick). On Nix:
#   nix shell nixpkgs#openbox nixpkgs#wmctrl nixpkgs#xdotool nixpkgs#imagemagick
set -eu

app=target/release/agents-cockpit-desktop
out=docs/images/app.png
[ -x "$app" ] || { echo "build $app first: cargo build --release -p cockpit_desktop" >&2; exit 1; }

# Other sessions may take screenshots on this machine: a display of their own each.
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

Xvfb ":$display" -screen 0 2560x1440x24 >/dev/null 2>&1 &
pids="$!"
sleep 1.5
kill -0 "$pids" 2>/dev/null || { echo "Xvfb did not start on :$display" >&2; exit 1; }
export DISPLAY=":$display"
openbox >/dev/null 2>&1 &
pids="$pids $!"
sleep 1
# The demo reads and writes no preferences, and its state stays in the scratch folder.
XDG_CONFIG_HOME="$work" XDG_STATE_HOME="$work" XDG_DATA_HOME="$work" XDG_CACHE_HOME="$work" \
    "$app" --demo --demo-showcase >"$work/app.log" 2>&1 &
pids="$pids $!"
sleep 15
window=$(wmctrl -l | awk 'NR == 1 { print $1 }')
[ -n "$window" ] || { echo "the app opened no window: $work/app.log" >&2; cat "$work/app.log" >&2; exit 1; }
wmctrl -i -r "$window" -b add,maximized_vert,maximized_horz
sleep 2
# The sidebar's right edge starts at x 258. A click in the sidebar's empty part takes the focus
# off the splitter, which would show highlighted.
xdotool mousemove 258 700 sleep 0.3 mousedown 1 sleep 0.2 mousemove 330 700 sleep 0.2 \
    mousemove 430 700 sleep 0.2 mouseup 1 sleep 1 mousemove 200 1300 click 1 sleep 0.5 \
    mousemove 1300 1430 sleep 1
import -window root "$work/shot.png"
wmctrl -i -c "$window"
# The window manager's title bar is 19 pixels high.
magick "$work/shot.png" -crop 2560x1421+0+19 +repage -strip -define png:compression-level=9 "$out"
echo "wrote $out"
