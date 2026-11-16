#!/bin/sh
# Installs the Yetack app from this tarball's folder into ~/.local, or into the folder --prefix
# names:
#   bin/yetack-desktop        the app
#   bin/yetack-x86_64-linux   the static daemon that the app sends to x86_64 servers
#   bin/yetack-aarch64-linux  the one it sends to ARM servers
#   bin/yetack                the x86_64 daemon again, which the service and the command line run
#   lib/yetack/libxdo.so.3    a library the app needs, which recent distros have only as libxdo.so.4
#   share/applications/yetack-desktop.desktop, and the icons under share/icons/hicolor
# Then it checks that the app finds every library it needs, and names the packages to install
# when it does not. --uninstall removes the same files.
#
# Exit status: 0 when the app finds its libraries, 3 when it is installed but some are missing,
# 2 for a wrong option, 1 for any other failure.
set -eu

prefix=$HOME/.local
action=install
while [ $# -gt 0 ]; do
    case $1 in
        --prefix)
            prefix=${2:?--prefix needs a folder}
            shift 2 ;;
        --prefix=*)
            prefix=${1#--prefix=}
            shift ;;
        --uninstall)
            action=uninstall
            shift ;;
        -h | --help)
            sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
            exit 0 ;;
        *)
            echo "unknown option: $1" >&2
            exit 2 ;;
    esac
done

if [ -z "$prefix" ]; then
    echo "--prefix needs a folder" >&2
    exit 2
fi
# The desktop entry needs the program's full path.
case $prefix in
    /*) ;;
    *) prefix=$(pwd)/$prefix ;;
esac
here=$(cd "$(dirname "$0")" && pwd)
bin=$prefix/bin
apps=$prefix/share/applications
icons=$prefix/share/icons/hicolor

if [ "$action" = uninstall ]; then
    rm -f "$bin/yetack-desktop" "$bin/yetack-x86_64-linux" "$bin/yetack-aarch64-linux" "$bin/yetack" \
        "$apps/yetack-desktop.desktop" "$icons/scalable/apps/yetack.svg" \
        "$icons/256x256/apps/yetack.png"
    rm -rf "$prefix/lib/yetack"
    echo "Removed Yetack from $prefix."
    exit 0
fi

# Copies $1 to $2 through a new file and a rename: a program that runs, such as the daemon of the
# background service, cannot be written over, but it can be replaced.
put() {
    mkdir -p "$(dirname "$2")"
    cp "$1" "$2.new"
    chmod "$3" "$2.new"
    mv -f "$2.new" "$2"
}

put "$here/yetack-desktop" "$bin/yetack-desktop" 755
put "$here/yetack-x86_64-linux" "$bin/yetack-x86_64-linux" 755
put "$here/yetack-aarch64-linux" "$bin/yetack-aarch64-linux" 755
put "$here/yetack-x86_64-linux" "$bin/yetack" 755
put "$here/lib/libxdo.so.3" "$prefix/lib/yetack/libxdo.so.3" 644
put "$here/yetack.svg" "$icons/scalable/apps/yetack.svg" 644
put "$here/yetack-256.png" "$icons/256x256/apps/yetack.png" 644
# The desktop's menu starts the app with the PATH of the session, which may not hold $bin. In a
# quoted Exec argument, the desktop entry spec writes \ as \\\\, " ` $ as \\" \\` \\$, % as %%.
program=$(printf '%s\n' "$bin/yetack-desktop" |
    sed -e 's/\\/\\\\\\\\/g' -e 's/["`$]/\\\\&/g' -e 's/%/%%/g')
entry=$(mktemp)
PROGRAM=$program awk '
    sub(/^Exec=yetack-desktop/, "") { $0 = "Exec=\"" ENVIRON["PROGRAM"] "\"" $0 }
    { print }
' "$here/yetack-desktop.desktop" > "$entry"
put "$entry" "$apps/yetack-desktop.desktop" 644
rm -f "$entry"
update-desktop-database "$apps" 2>/dev/null || true
gtk-update-icon-cache -q -t "$icons" 2>/dev/null || true
echo "Installed Yetack in $prefix."

case ":$PATH:" in
    *":$bin:"*) ;;
    *) echo "To run yetack and yetack-desktop from a terminal, add $bin to your PATH." ;;
esac

if ! command -v ldd >/dev/null 2>&1; then
    echo "ldd is not installed: the libraries the app needs were not checked."
    exit 0
fi
missing=$(ldd "$bin/yetack-desktop" 2>&1 | grep 'not found' | sed 's/^[[:space:]]*//') || true
if [ -z "$missing" ]; then
    echo "The app finds every library it needs. Start it from your desktop's menu, or run yetack-desktop."
    exit 0
fi

echo
echo "The app cannot start yet. These libraries are missing:"
printf '%s\n' "$missing" | sed 's/^/  /'
system=
if [ -r /etc/os-release ]; then
    # shellcheck disable=SC1091
    system=$(. /etc/os-release && echo "${ID:-} ${ID_LIKE:-}")
fi
case " $system " in
    *" gentoo "*)
        hint="sudo emerge --ask --noreplace net-libs/webkit-gtk:4.1 x11-libs/gtk+:3 net-libs/libsoup:3.0 x11-libs/libXtst x11-libs/libXinerama x11-libs/libxkbcommon" ;;
    *" fedora "* | *" rhel "*)
        hint="sudo dnf install webkit2gtk4.1 gtk3 libsoup3 libXtst libXinerama libxkbcommon (the release's .rpm installs them for you)" ;;
    *" arch "*)
        hint="sudo pacman -S --needed webkit2gtk-4.1 gtk3 libsoup3 libxtst libxinerama libxkbcommon" ;;
    *" suse "* | *" opensuse "*)
        hint="sudo zypper install libwebkit2gtk-4_1-0 libgtk-3-0 libsoup-3_0-0 libXtst6 libXinerama1 libxkbcommon0" ;;
    *" debian "* | *" ubuntu "*)
        hint="sudo apt install libwebkit2gtk-4.1-0 libgtk-3-0 libsoup-3.0-0 libxtst6 libxinerama1 libxkbcommon0 (the release's .deb installs them for you)" ;;
    *)
        hint="" ;;
esac
if [ -n "$hint" ]; then
    echo "Install them with:"
    echo "  $hint"
else
    echo "Install WebKitGTK 4.1, GTK 3, libsoup 3, and the X11 libraries Xtst, Xinerama and xkbcommon with your system's packages."
fi
echo "A 'GLIBC_2.xx not found' line means this system is older than the app: it needs glibc 2.35 or newer."
exit 3
