//! The Linux daemon this app carries, and its trip to a server: the app sends the file over SSH,
//! then the update script checks its checksum and version and installs it.

use std::io;
use std::path::{Path, PathBuf};

use cockpit_client::{ssh_inline_command, ssh_script_command};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::state::daemon_probe::ServerInstall;
use crate::state::daemon_update::OutputLine;
use crate::state::error_chain;
use crate::update_script::{self, APP_VERSION};

const INSTALL_SCRIPT: &str = include_str!("../../../contrib/install-daemon.sh");

/// The Linux daemon's name beside the app. The Windows zip holds it, and `just linux-daemon`
/// writes it beside a build from source.
pub const BESIDE_THE_APP: &str = "agents-cockpit-x86_64-linux";

/// The Linux daemon a package carries, set when it builds the app: the `.deb`'s
/// `/usr/bin/agents-cockpit`, or the flake's static daemon in the Nix store.
const PACKAGED: Option<&str> = option_env!("AGENTS_COCKPIT_LINUX_DAEMON");

/// The bytes sent at once, between two reports of the progress.
const CHUNK: usize = 256 * 1024;

/// Writes stdin to a new file on the server, then prints its path. The folder is the user's own,
/// and a send cut short leaves a file there, so each send deletes those older than an hour. Like
/// `PROBE_SCRIPT`, it holds no single quote and no backslash: Windows' ssh breaks the quoting of a
/// remote command that has one.
const RECEIVE_SCRIPT: &str = r#"set -eu
umask 077
dir=${XDG_CACHE_HOME:-$HOME/.cache}/agents-cockpit/sent
mkdir -p "$dir"
find "$dir" -type f -mmin +60 -exec rm -f {} + 2>/dev/null || true
file=$(mktemp "$dir/agents-cockpit.XXXXXX")
if ! cat >"$file"; then
    rm -f "$file"
    exit 1
fi
echo "$file""#;

/// Prints the server's system, then where the daemon `$1` names is, who owns it, and its version,
/// as `key=value` lines for `ServerInstall::parse`. A command run over SSH loads no shell profile,
/// so the folders a user's own programs are in join the `PATH`.
const PROBE_SCRIPT: &str = r#"PATH=$PATH:$HOME/.local/share/agents-cockpit/bin:$HOME/.local/bin:$HOME/.cargo/bin:$HOME/.nix-profile/bin:/nix/var/nix/profiles/default/bin:/run/current-system/sw/bin:/etc/profiles/per-user/$(id -un)/bin
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
case $path in
    /nix/store/*) owner=nix ;;
    *)
        if command -v dpkg >/dev/null 2>&1; then
            package=$(dpkg -S "$path" 2>/dev/null | head -n 1 | cut -d : -f 1)
        fi
        if [ -n "$package" ]; then
            owner="package $package"
        elif [ -w "$folder" ] && {
            [ "$folder" = "$(readlink -f "$HOME/.local/share/agents-cockpit/bin")" ] ||
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
        "--built",
        sent.remote_file.as_str(),
        "--sha256",
        sent.sha256.as_str(),
        "--version",
        version.as_str(),
    ];
    run_install_script(host, &arguments, on_lines).await
}

/// Reads again what the daemon on `host` needs besides itself, giving the lines to `on_lines`.
pub async fn check_needs(host: &str, on_lines: impl FnMut(Vec<OutputLine>)) -> Option<i32> {
    run_install_script(host, &["--check"], on_lines).await
}

async fn run_install_script(
    host: &str,
    arguments: &[&str],
    mut on_lines: impl FnMut(Vec<OutputLine>),
) -> Option<i32> {
    let command = ssh_script_command(host, arguments).ok()?;
    update_script::run(command, INSTALL_SCRIPT, &mut on_lines)
        .await
        .ok()?
        .code()
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
    ServerInstall::parse(&String::from_utf8_lossy(&output.stdout))
}

/// The Linux daemon this app carries, when it has one: the one its package names, or the file
/// beside the app.
pub fn carried() -> Option<PathBuf> {
    let packaged = PACKAGED.filter(|path| !path.is_empty()).map(PathBuf::from);
    let beside = std::env::current_exe()
        .ok()
        .and_then(|app| Some(app.parent()?.join(BESIDE_THE_APP)));
    packaged
        .into_iter()
        .chain(beside)
        .find(|path| path.is_file())
}

/// A daemon the app sent, as a file on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentDaemon {
    /// Its absolute path on the server.
    pub remote_file: String,
    /// Its SHA-256 as the app read it, in lowercase hex.
    pub sha256: String,
}

/// Sends `file` to `host`, telling `on_progress` the bytes sent and the total as it goes. `Err`
/// says why it failed.
pub async fn send(
    host: &str,
    file: &Path,
    mut on_progress: impl FnMut(u64, u64),
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
        on_progress(sent, total);
        for chunk in bytes.chunks(CHUNK) {
            stdin.write_all(chunk).await?;
            sent = sent.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
            on_progress(sent, total);
        }
        stdin.shutdown().await
    };
    let mut said = String::new();
    let mut errors = String::new();
    let (written, _, _) = tokio::join!(
        writing,
        stdout.read_to_string(&mut said),
        stderr.read_to_string(&mut errors),
    );
    let status = child.wait().await.map_err(|error| error_chain(&error))?;
    let remote_file = said.lines().last().unwrap_or_default().trim();
    if status.success() && written.is_ok() && remote_file.starts_with('/') {
        return Ok(SentDaemon {
            remote_file: remote_file.to_owned(),
            sha256,
        });
    }
    Err(send_failure(&errors, written.err()))
}

/// Why a send failed: what ssh or the server wrote, else why the app could not write.
fn send_failure(errors: &str, write_error: Option<io::Error>) -> String {
    let errors = errors.trim();
    if !errors.is_empty() {
        return errors.to_owned();
    }
    match write_error {
        Some(error) => format!("The connection closed during the send: {error}"),
        None => "The server did not say where it wrote the file".to_owned(),
    }
}

fn hex(bytes: &[u8]) -> String {
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
    fn a_failed_send_says_what_the_server_wrote_first() {
        let refused = io::Error::from(io::ErrorKind::BrokenPipe);

        assert_eq!(
            send_failure("mkdir: cannot create directory\n", Some(refused)),
            "mkdir: cannot create directory"
        );
        assert!(
            send_failure("", Some(io::Error::from(io::ErrorKind::BrokenPipe)))
                .starts_with("The connection closed during the send")
        );
        assert_eq!(
            send_failure("  ", None),
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
        ServerInstall::parse(&String::from_utf8(output.stdout).unwrap()).unwrap()
    }

    /// A fake daemon at `path`, which prints its version.
    #[cfg(target_os = "linux")]
    fn fake_daemon(path: &Path) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\necho 'agents-cockpit 0.1.0 (743d664)'\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_probe_owns_a_daemon_in_the_users_bin_folder_and_reads_its_version() {
        use crate::state::daemon_probe::DaemonOwner;

        let home = tempfile::tempdir().unwrap();
        let daemon = home.path().join(".local/bin/agents-cockpit");
        fake_daemon(&daemon);

        let install = probe_here(home.path(), daemon.to_str().unwrap());

        let found = install.replaceable().unwrap();
        assert_eq!(found.owner, DaemonOwner::App);
        assert_eq!(found.version, "agents-cockpit 0.1.0 (743d664)");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_probe_leaves_a_daemon_elsewhere_or_behind_a_link_to_elsewhere_to_the_user() {
        use crate::state::daemon_probe::DaemonOwner;

        let home = tempfile::tempdir().unwrap();
        let elsewhere = home.path().join("tools/agents-cockpit");
        fake_daemon(&elsewhere);
        let link = home.path().join(".local/bin/agents-cockpit");
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

        let install = probe_here(home.path(), "/nowhere/agents-cockpit");

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
        assert!(path.starts_with(cache.path().join("agents-cockpit/sent")));
        assert_eq!(std::fs::read(path).unwrap(), b"the daemon");
    }
}
