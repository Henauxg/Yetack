//! The Linux daemon this app carries, and its trip to a server: the app sends the file over SSH,
//! then the update script checks its checksum and version and installs it.

use std::io;
use std::path::{Path, PathBuf};

use cockpit_client::{ssh_inline_command, ssh_script_command};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::state::daemon_probe::{LinuxCpu, ServerInstall};
use crate::state::daemon_update::OutputLine;
use crate::state::error_chain;
use crate::state::update_plan::{CarriedDaemons, SendProgress};
use crate::update_script::{
    self, APP_VERSION, BINARY_OPTION, BOOT_START_OPTION, BUILT_OPTION, CHECK_OPTION, SHA256_OPTION,
    START_OPTION, VERSION_OPTION,
};
use crate::{release_daemon, self_update};

const INSTALL_SCRIPT: &str = include_str!("../../../contrib/install-daemon.sh");

/// The x86_64 Linux daemon a package carries, set when it builds the app: the `.deb`'s
/// `/usr/bin/yetack`, or the flake's static daemon in the Nix store.
const PACKAGED: Option<&str> = option_env!("YETACK_LINUX_DAEMON");

/// The aarch64 Linux daemon a package carries, set when it builds the app.
const PACKAGED_AARCH64: Option<&str> = option_env!("YETACK_LINUX_DAEMON_AARCH64");

/// The bytes sent at once, between two reports of the progress.
const CHUNK: usize = 256 * 1024;

/// Writes stdin to a new file on the server, then prints its path. The folder is the user's own,
/// and a send cut short leaves a file there, so each send deletes those older than an hour. Like
/// `PROBE_SCRIPT`, it holds no single quote and no backslash: Windows' ssh breaks the quoting of a
/// remote command that has one.
const RECEIVE_SCRIPT: &str = r#"set -eu
umask 077
dir=${XDG_CACHE_HOME:-$HOME/.cache}/yetack/sent
mkdir -p "$dir"
find "$dir" -type f -mmin +60 -exec rm -f {} + 2>/dev/null || true
file=$(mktemp "$dir/yetack.XXXXXX")
if ! cat >"$file"; then
    rm -f "$file"
    exit 1
fi
echo "$file""#;

/// Prints the server's system, then where the daemon `$1` names is, who owns it, and its version,
/// as `key=value` lines that `ServerInstall` parses: `ProbeKey` names the keys, and `OwnerWord`,
/// `PACKAGE_OWNER` and `PackageFormat` the owners. A command run over SSH loads no shell profile,
/// so the folders a user's own programs are in join the `PATH`.
const PROBE_SCRIPT: &str = r#"PATH=$PATH:$HOME/.local/share/yetack/bin:$HOME/.local/bin:$HOME/.cargo/bin:$HOME/.nix-profile/bin:/nix/var/nix/profiles/default/bin:/run/current-system/sw/bin:/etc/profiles/per-user/$(id -un)/bin
echo "system=$(uname -sm)"
binary=$1
case $binary in
    /*) ;;
    *) binary=$(command -v "$binary" 2>/dev/null) ;;
esac
path=
[ -z "$binary" ] || path=$(readlink -f "$binary" 2>/dev/null)
if [ -z "$path" ] || [ ! -f "$path" ]; then
    echo owner=missing
    exit 0
fi
echo "path=$path"
echo "command=$binary"
folder=${path%/*}
package=
format=
case $path in
    /nix/store/*) owner=nix ;;
    *)
        if command -v dpkg >/dev/null 2>&1; then
            package=$(dpkg -S "$path" 2>/dev/null | head -n 1 | cut -d : -f 1)
            format=deb
        fi
        if [ -z "$package" ] && command -v rpm >/dev/null 2>&1; then
            package=$(rpm -qf --queryformat %{NAME} "$path" 2>/dev/null) || package=
            format=rpm
        fi
        if [ -n "$package" ]; then
            owner="package $format $package"
        elif [ -w "$folder" ] && {
            [ "$folder" = "$(readlink -f "$HOME/.local/share/yetack/bin")" ] ||
                [ "$folder" = "$(readlink -f "$HOME/.local/bin")" ]
        }; then
            owner=app
        else
            owner=other
        fi
        ;;
esac
echo "owner=$owner"
echo "version=$("$path" --version 2>/dev/null | head -n 1)"
"#;

/// Installs `sent` on `host` as the daemon's service, giving the script's lines to `on_lines`.
/// Returns the script's exit code: `None` when ssh lost the connection or could not start.
pub async fn install(
    host: &str,
    sent: &SentDaemon,
    on_lines: impl FnMut(Vec<OutputLine>),
) -> Option<i32> {
    let version = match update_script::app_commit() {
        Some(commit) => cockpit_daemon::version_text(APP_VERSION, Some(&commit)),
        None => APP_VERSION.to_owned(),
    };
    let arguments = [
        BUILT_OPTION,
        sent.remote_file.as_str(),
        SHA256_OPTION,
        sent.sha256.as_str(),
        VERSION_OPTION,
        version.as_str(),
    ];
    run_install_script(host, &arguments, on_lines).await
}

/// Starts the daemon at `binary` on `host`: its systemd unit when it has one, else on its own.
pub async fn start_daemon(
    host: &str,
    binary: &str,
    on_lines: impl FnMut(Vec<OutputLine>),
) -> Option<i32> {
    run_install_script(host, &[START_OPTION, binary], on_lines).await
}

/// Adds the daemon's start at boot to the user's crontab on `host`, then reads what it needs. The
/// line starts `binary`, or the daemon the install put in place without one.
pub async fn add_boot_start(
    host: &str,
    binary: Option<&str>,
    on_lines: impl FnMut(Vec<OutputLine>),
) -> Option<i32> {
    run_install_script(host, &with_binary(BOOT_START_OPTION, binary), on_lines).await
}

/// Reads again what the daemon on `host` needs besides itself, giving the lines to `on_lines`. Its
/// start command and crontab line name `binary`, or the daemon the install put in place without
/// one.
pub async fn check_needs(
    host: &str,
    binary: Option<&str>,
    on_lines: impl FnMut(Vec<OutputLine>),
) -> Option<i32> {
    run_install_script(host, &with_binary(CHECK_OPTION, binary), on_lines).await
}

/// The install script's `option`, then `binary` after `BINARY_OPTION` when there is one.
fn with_binary<'a>(option: &'a str, binary: Option<&'a str>) -> Vec<&'a str> {
    let mut arguments = vec![option];
    arguments.extend(
        binary
            .into_iter()
            .flat_map(|binary| [BINARY_OPTION, binary]),
    );
    arguments
}

/// Runs the install script with `arguments`. Why ssh could not start or the script could not run
/// goes to `on_lines` as a line of stderr, and the exit code is then `None`.
async fn run_install_script(
    host: &str,
    arguments: &[&str],
    mut on_lines: impl FnMut(Vec<OutputLine>),
) -> Option<i32> {
    let command = match ssh_script_command(host, arguments) {
        Ok(command) => command,
        Err(error) => {
            on_lines(vec![update_script::error_line(&error)]);
            return None;
        }
    };
    match update_script::run(command, INSTALL_SCRIPT, &mut on_lines).await {
        Ok(status) => status.code(),
        Err(error) => {
            on_lines(vec![update_script::error_line(&error)]);
            None
        }
    }
}

/// Reads how `host` runs `binary`, the daemon's path or command. `Err` says why it failed.
pub async fn probe(host: &str, binary: &str) -> Result<ServerInstall, String> {
    let output = ssh_inline_command(host, PROBE_SCRIPT, &[binary])
        .map_err(|error| error_chain(&error))?
        .output()
        .await
        .map_err(|error| error_chain(&error))?;
    if !output.status.success() {
        let errors = String::from_utf8_lossy(&output.stderr);
        return Err(match errors.trim() {
            "" => format!("ssh ended with {}", output.status),
            said => said.to_owned(),
        });
    }
    let install: ServerInstall = String::from_utf8_lossy(&output.stdout).parse()?;
    Ok(install)
}

/// The Linux daemons this app carries, for each CPU it has one for, and whether it may download
/// its release's for the others. It asks GitHub only when a CPU has none carried.
pub async fn carried_daemons() -> CarriedDaemons {
    let mut daemons = carried_daemons_now();
    let lacks_one = daemons.x86_64.is_none() || daemons.aarch64.is_none();
    if lacks_one && update_script::app_commit().is_some() {
        daemons.release = release_daemon::is_release_build().await;
    }
    daemons
}

/// `carried_daemons` without asking GitHub: whether the app may download comes from what it
/// learned before.
pub fn carried_daemons_now() -> CarriedDaemons {
    CarriedDaemons {
        x86_64: carried(LinuxCpu::X86_64),
        aarch64: carried(LinuxCpu::Aarch64),
        release: release_daemon::known_release_build(),
    }
}

/// The Linux daemon this app carries for `cpu`, when it has one: the one its package names, or the
/// file beside the app.
pub fn carried(cpu: LinuxCpu) -> Option<PathBuf> {
    let packaged = match cpu {
        LinuxCpu::X86_64 => PACKAGED,
        LinuxCpu::Aarch64 => PACKAGED_AARCH64,
    };
    let packaged = packaged.filter(|path| !path.is_empty()).map(PathBuf::from);
    let beside = std::env::current_exe()
        .ok()
        .and_then(|app| carried_beside(&app, cpu));
    packaged
        .into_iter()
        .chain(beside)
        .find(|path| path.is_file())
}

/// The daemon for `cpu` beside `app`, the app's program. Between an update's move and the next
/// start, the app's program and each daemon it had wait as `<name>.old`, and the files of their
/// names are the new release's. A daemon the app did not have is then the new release's alone, so
/// the app carries none for that CPU.
fn carried_beside(app: &Path, cpu: LinuxCpu) -> Option<PathBuf> {
    let folder = app.parent()?;
    let name = cpu.daemon_file_name();
    let own = folder.join(format!("{name}{}", self_update::OLD_SUFFIX));
    if own.is_file() {
        return Some(own);
    }
    let updated = self_update::with_suffix(app, self_update::OLD_SUFFIX).is_file();
    (!updated).then(|| folder.join(name))
}

/// A daemon the app sent, as a file on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentDaemon {
    /// Its absolute path on the server.
    pub remote_file: String,
    /// Its SHA-256 as the app read it, in lowercase hex.
    pub sha256: String,
}

/// Sends `file` to `host`, telling `on_progress` how far it has gone. `Err` says why it failed.
pub async fn send(
    host: &str,
    file: &Path,
    mut on_progress: impl FnMut(SendProgress),
) -> Result<SentDaemon, String> {
    let bytes = tokio::fs::read(file)
        .await
        .map_err(|error| format!("Cannot read {}: {}", file.display(), error_chain(&error)))?;
    let sha256 = hex(&Sha256::digest(&bytes));
    let mut command =
        ssh_inline_command(host, RECEIVE_SCRIPT, &[]).map_err(|error| error_chain(&error))?;
    let mut child = command.spawn().map_err(|error| error_chain(&error))?;
    let (Some(mut stdin), Some(mut stdout), Some(mut stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err("ssh started without its pipes".to_owned());
    };
    let total = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    // ssh may end before it reads everything, as when the host refuses it: its exit status and
    // its stderr then say why.
    let writing = async move {
        let mut sent = 0_u64;
        on_progress(SendProgress { sent, total });
        for chunk in bytes.chunks(CHUNK) {
            stdin.write_all(chunk).await?;
            sent = sent.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
            on_progress(SendProgress { sent, total });
        }
        stdin.shutdown().await
    };
    let mut said = String::new();
    let mut errors = String::new();
    let (written, read_said, read_errors) = tokio::join!(
        writing,
        stdout.read_to_string(&mut said),
        stderr.read_to_string(&mut errors),
    );
    let status = child.wait().await.map_err(|error| error_chain(&error))?;
    let remote_file = said.lines().last().unwrap_or_default().trim();
    let complete = written.is_ok() && read_said.is_ok();
    if status.success() && complete && remote_file.starts_with('/') {
        return Ok(SentDaemon {
            remote_file: remote_file.to_owned(),
            sha256,
        });
    }
    let own_error = own_send_error(written, read_said, read_errors);
    Err(send_failure(&errors, own_error))
}

/// What failed on the app's side of a send: its write, its read of the file's path, then its
/// read of what ssh said.
fn own_send_error(
    written: io::Result<()>,
    read_said: io::Result<usize>,
    read_errors: io::Result<usize>,
) -> Option<String> {
    if let Err(error) = written {
        return Some(format!("The connection closed during the send: {error}"));
    }
    if let Err(error) = read_said {
        return Some(format!(
            "Cannot read where the server wrote the file: {error}"
        ));
    }
    read_errors
        .err()
        .map(|error| format!("Cannot read what ssh said: {error}"))
}

/// Why a send failed: what ssh or the server wrote, else what failed on the app's side.
fn send_failure(errors: &str, own_error: Option<String>) -> String {
    let errors = errors.trim();
    if !errors.is_empty() {
        return errors.to_owned();
    }
    own_error.unwrap_or_else(|| "The server did not say where it wrote the file".to_owned())
}

/// The bytes in lowercase hex, as `sha256sum` prints a checksum.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checksum_is_in_lowercase_hex_as_sha256sum_prints_it() {
        assert_eq!(
            hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn the_embedded_install_script_has_no_carriage_return() {
        assert!(!INSTALL_SCRIPT.contains('\r'));
    }

    #[test]
    fn the_inline_scripts_hold_nothing_that_windows_ssh_breaks() {
        for script in [RECEIVE_SCRIPT, PROBE_SCRIPT] {
            assert!(!script.contains(['\'', '\\']), "{script}");
        }
    }

    #[test]
    fn the_probe_prints_each_key_and_owner_that_server_install_reads() {
        use crate::state::daemon_probe::{OwnerWord, PACKAGE_OWNER, PackageFormat, ProbeKey};
        use strum::VariantArray;

        for &key in ProbeKey::VARIANTS {
            let key: &str = key.into();
            assert!(PROBE_SCRIPT.contains(&format!("echo \"{key}=")), "{key}");
        }
        for &owner in OwnerWord::VARIANTS {
            let owner: &str = owner.into();
            assert!(PROBE_SCRIPT.contains(&format!("owner={owner}")), "{owner}");
        }
        assert!(PROBE_SCRIPT.contains(&format!("owner=\"{PACKAGE_OWNER}$format ")));
        for &format in PackageFormat::VARIANTS {
            let format: &str = format.into();
            assert!(
                PROBE_SCRIPT.contains(&format!("format={format}\n")),
                "{format}"
            );
        }
    }

    #[test]
    fn the_probe_owns_a_daemon_in_the_folders_the_screens_name() {
        use crate::state::daemon_probe::{APP_BIN_FOLDER, USER_BIN_FOLDER};

        for folder in [APP_BIN_FOLDER, USER_BIN_FOLDER] {
            let in_home = folder.strip_prefix("~/").unwrap();
            assert!(
                PROBE_SCRIPT.contains(&format!("readlink -f \"$HOME/{in_home}\"")),
                "{folder}"
            );
        }
    }

    #[test]
    fn the_install_writes_the_files_the_screens_name() {
        use cockpit_daemon::PROGRAM;

        use crate::state::daemon_probe::{APP_BIN_FOLDER, UNIT_FILE};

        let folder = APP_BIN_FOLDER.strip_prefix("~/").unwrap();
        let exec_start = format!("ExecStart=%h/{folder}/{PROGRAM} serve");
        assert!(INSTALL_SCRIPT.contains(&exec_start), "{exec_start}");
        let unit = UNIT_FILE.strip_prefix("~/.config/").unwrap();
        let unit_path = format!("${{XDG_CONFIG_HOME:-$HOME/.config}}/{unit}");
        assert!(INSTALL_SCRIPT.contains(&unit_path), "{unit_path}");
    }

    #[test]
    fn a_failed_send_says_what_the_server_wrote_first() {
        let refused = || Err(io::Error::from(io::ErrorKind::BrokenPipe));
        let invalid = || Err(io::Error::from(io::ErrorKind::InvalidData));

        assert_eq!(
            send_failure(
                "mkdir: cannot create directory\n",
                own_send_error(refused(), Ok(0), Ok(0))
            ),
            "mkdir: cannot create directory"
        );
        assert!(
            send_failure("", own_send_error(refused(), invalid(), Ok(0)))
                .starts_with("The connection closed during the send")
        );
        assert!(
            send_failure("", own_send_error(Ok(()), invalid(), Ok(0)))
                .starts_with("Cannot read where the server wrote the file")
        );
        assert!(
            send_failure("", own_send_error(Ok(()), Ok(0), invalid()))
                .starts_with("Cannot read what ssh said")
        );
        assert_eq!(
            send_failure("  ", own_send_error(Ok(()), Ok(0), Ok(0))),
            "The server did not say where it wrote the file"
        );
    }

    /// Runs the probe on this machine, for the daemon `binary` names, with `home` as `$HOME`.
    #[cfg(target_os = "linux")]
    fn probe_here(home: &Path, binary: &str) -> ServerInstall {
        let output = std::process::Command::new("sh")
            .args(["-c", PROBE_SCRIPT, "sh", binary])
            .env("HOME", home)
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap().parse().unwrap()
    }

    /// A fake daemon at `path`, which prints its version.
    #[cfg(target_os = "linux")]
    fn fake_daemon(path: &Path) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\necho 'yetack 0.1.0 (743d664)'\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_probe_owns_a_daemon_in_the_users_bin_folder_and_reads_its_version() {
        use crate::state::daemon_probe::DaemonOwner;

        let home = tempfile::tempdir().unwrap();
        let daemon = home.path().join(".local/bin/yetack");
        fake_daemon(&daemon);

        let install = probe_here(home.path(), daemon.to_str().unwrap());

        let found = install.replaceable().unwrap();
        assert_eq!(found.owner, DaemonOwner::App);
        assert_eq!(found.version, "yetack 0.1.0 (743d664)");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_probe_leaves_a_daemon_elsewhere_or_behind_a_link_to_elsewhere_to_the_user() {
        use crate::state::daemon_probe::DaemonOwner;

        let home = tempfile::tempdir().unwrap();
        let elsewhere = home.path().join("tools/yetack");
        fake_daemon(&elsewhere);
        let link = home.path().join(".local/bin/yetack");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();

        for binary in [&elsewhere, &link] {
            let install = probe_here(home.path(), binary.to_str().unwrap());

            let found = install.daemon.unwrap();
            assert_eq!(found.owner, DaemonOwner::Other, "{}", binary.display());
            assert_eq!(found.path, elsewhere.to_str().unwrap());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_probe_says_when_no_daemon_is_there() {
        let home = tempfile::tempdir().unwrap();

        let install = probe_here(home.path(), "/nowhere/yetack");

        assert_eq!(install.daemon, None);
    }

    #[cfg(unix)]
    #[test]
    fn the_receive_script_writes_its_stdin_to_a_new_file_and_prints_its_path() {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let cache = tempfile::tempdir().unwrap();
        let mut child = Command::new("sh")
            .args(["-c", RECEIVE_SCRIPT])
            .env("XDG_CACHE_HOME", cache.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"the daemon")
            .unwrap();
        let output = child.wait_with_output().unwrap();

        assert!(output.status.success());
        let path = String::from_utf8(output.stdout).unwrap();
        let path = Path::new(path.trim());
        assert!(path.starts_with(cache.path().join("yetack/sent")));
        assert_eq!(std::fs::read(path).unwrap(), b"the daemon");
    }

    #[test]
    fn an_update_waiting_for_the_restart_carries_the_daemon_it_moved_aside() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path();
        let app = folder.join("yetack-desktop.exe");
        std::fs::write(&app, "app").unwrap();
        std::fs::write(folder.join("yetack-x86_64-linux"), "daemon").unwrap();
        assert_eq!(
            carried_beside(&app, LinuxCpu::X86_64),
            Some(folder.join("yetack-x86_64-linux"))
        );
        assert_eq!(
            carried_beside(&app, LinuxCpu::Aarch64),
            Some(folder.join("yetack-aarch64-linux"))
        );

        std::fs::write(folder.join("yetack-desktop.exe.old"), "this app").unwrap();
        std::fs::write(folder.join("yetack-aarch64-linux"), "new daemon").unwrap();
        for cpu in [LinuxCpu::X86_64, LinuxCpu::Aarch64] {
            let old = folder.join(format!("{}.old", cpu.daemon_file_name()));
            std::fs::write(&old, "this app's daemon").unwrap();
            assert_eq!(carried_beside(&app, cpu), Some(old), "{cpu:?}");
        }
    }

    /// A zip that carried no ARM daemon gets one with the update. Until the restart, that one is
    /// the new release's, which a server refuses from this app.
    #[test]
    fn an_update_waiting_for_the_restart_carries_no_daemon_the_app_lacked() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path();
        let app = folder.join("yetack-desktop.exe");
        std::fs::write(&app, "new app").unwrap();
        std::fs::write(folder.join("yetack-desktop.exe.old"), "this app").unwrap();
        std::fs::write(folder.join("yetack-x86_64-linux"), "new daemon").unwrap();
        let own = folder.join("yetack-x86_64-linux.old");
        std::fs::write(&own, "this app's daemon").unwrap();
        std::fs::write(folder.join("yetack-aarch64-linux"), "new daemon").unwrap();

        assert_eq!(carried_beside(&app, LinuxCpu::X86_64), Some(own));
        assert_eq!(carried_beside(&app, LinuxCpu::Aarch64), None);
    }
}
