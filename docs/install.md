# Install

The [README](../README.md) lists the release files for each computer. This page has the steps for the tarball and for Nix. To build the app yourself, see [install-from-source.md](./install-from-source.md).

## Any other Linux

The tarball holds the app and the daemon, built for any Linux x86_64 with glibc 2.35 or newer. The app draws with your system's WebKitGTK 4.1, so install it first with your packages. On Gentoo, for example:

```sh
sudo emerge --ask --noreplace net-libs/webkit-gtk:4.1
```

Then unpack the tarball and run its `install.sh`, which copies the app, the daemon and the menu entry to `~/.local`:

```sh
tar xzf yetack-desktop-<version>-x86_64-linux.tar.gz
./yetack-desktop-<version>-x86_64-linux/install.sh
```

The script then checks that the app finds every library it needs. When one is missing, it names it, with the command that installs it on your distribution. `install.sh --prefix <folder>` installs elsewhere, and `install.sh --uninstall` removes the files. For a new release, do the same with the new tarball.

## With Nix

The repository is a Nix flake. Nix builds the app from the release's source on your computer, which takes a few minutes. The app, its menu entry and its icon then come with your Nix setup.

To try the app, add it to your user profile. If your Nix does not know `nix profile add`, write `install` in place of `add`.

```sh
nix profile add 'github:Henauxg/yetack/v<version>'
```

The profile keeps the version you added. For a new release, remove the app with `nix profile remove yetack`, then add the new version.

To install the app with the rest of your NixOS configuration, add the flake as an input of your system's flake:

```nix
# In your system's flake.nix
inputs.yetack.url = "github:Henauxg/yetack/v<version>";

# In your NixOS configuration
environment.systemPackages = [ inputs.yetack.packages.${pkgs.system}.default ];
```

For a new release, change the version in the input, run `nix flake update yetack`, and rebuild your system. A home-manager configuration takes the same package in `home.packages`.
