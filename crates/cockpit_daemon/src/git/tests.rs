//! The git requests run no program that a checkout's config or attributes name, which the agent
//! can write.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use cockpit_protocol::{CommitTexts, ErrorCode, GitFile};

use crate::git::test_repo::TestRepo;
use crate::git::{blame, file, log, show, status};

/// A folder of marker files, each of which a program the checkout names writes when git runs it.
struct Markers {
    folder: PathBuf,
}

impl Markers {
    fn new(repo: &TestRepo) -> Self {
        let folder = repo.home().join("markers");
        std::fs::create_dir(&folder).unwrap();
        Self { folder }
    }

    /// A shell command that writes the marker `name`, then runs `then`. Git for Windows runs it
    /// with its own `sh`, which takes `/` between names.
    fn command(&self, name: &str, then: &str) -> String {
        format!("echo ran > '{}'; {then}", slashed(&self.folder.join(name)))
    }

    fn written(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

fn slashed(path: &Path) -> String {
    path.to_str().unwrap().replace('\\', "/")
}

/// Writes `text` at `path`, dated a minute later, so that its time differs from the index's
/// whatever the file system's precision.
fn write_later(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    let later = SystemTime::now() + Duration::from_secs(60);
    let file = File::options().write(true).open(path).unwrap();
    file.set_modified(later).unwrap();
}

/// Gives HEAD a signature, so that `log.showSignature` makes `git log` check it with
/// `gpg.program`.
fn sign_head(repo: &TestRepo) {
    let unsigned = repo.git(&["cat-file", "commit", "HEAD"]);
    let (headers, message) = unsigned.split_once("\n\n").unwrap();
    let signed = format!(
        "{headers}\ngpgsig -----BEGIN PGP SIGNATURE-----\n \n iQEzBAABCAAdFiEE\n -----END PGP SIGNATURE-----\n\n{message}"
    );
    let commit_file = repo.home().join("signed-commit");
    std::fs::write(&commit_file, signed).unwrap();
    let hash = repo.git(&[
        "hash-object",
        "-t",
        "commit",
        "-w",
        commit_file.to_str().unwrap(),
    ]);
    repo.git(&["update-ref", "HEAD", hash.trim()]);
}

/// A program for `gpg.program`, which git runs without a shell.
fn gpg_program(markers: &Markers) -> PathBuf {
    let program = markers.folder.parent().unwrap().join("gpg.sh");
    write_script(&program, &markers.command("gpg", "true"));
    program
}

/// Writes a shell script at `path` that runs `command`. Git for Windows runs a hook that starts
/// with `#!`, and git on Unix one that may be executed.
fn write_script(path: &Path, command: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{command}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(path, Permissions::from_mode(0o755)).unwrap();
    }
}

#[tokio::test]
async fn no_request_runs_a_program_the_checkout_names() {
    let repo = TestRepo::new();
    let markers = Markers::new(&repo);
    repo.write(
        ".gitattributes",
        "*.txt filter=trap diff=trap\n*.dat filter=process\n",
    );
    repo.write("notes.txt", "one\ntwo\n");
    repo.write("data.dat", "data\n");
    repo.commit("Start");
    sign_head(&repo);
    let head = repo.head();
    let gpg = gpg_program(&markers);
    let settings = [
        ("filter.trap.clean", markers.command("clean", "cat")),
        ("filter.trap.smudge", markers.command("smudge", "cat")),
        ("filter.trap.required", "true".to_owned()),
        ("filter.process.process", markers.command("process", "cat")),
        ("diff.trap.textconv", markers.command("textconv", "cat")),
        ("diff.trap.command", markers.command("diff-command", "true")),
        ("diff.external", markers.command("diff-external", "true")),
        ("core.fsmonitor", markers.command("fsmonitor", "false")),
        ("core.pager", markers.command("pager", "cat")),
        ("log.showSignature", "true".to_owned()),
        ("gpg.program", slashed(&gpg)),
    ];
    for (key, value) in &settings {
        repo.git(&["config", key, value]);
    }
    // A file of the same size but a later time: git reads it, through its filter, to tell
    // whether it changed.
    write_later(&repo.root.join("notes.txt"), "ONE\ntwo\n");
    write_later(&repo.root.join("data.dat"), "DATA\n");
    let ask = || (repo.root.clone(), Vec::new(), Some(repo.home().to_owned()));
    let notes = || repo.root.join("notes.txt");

    let (folder, touched, home) = ask();
    let committed = file(folder, touched, home, notes()).await.unwrap();
    let (folder, touched, home) = ask();
    let blamed = blame(folder, touched, home, notes()).await.unwrap();
    let (folder, touched, home) = ask();
    let logged = log(folder, touched, home, notes()).await.unwrap();
    let (folder, touched, home) = ask();
    let shown = show(folder, touched, home, notes(), head.clone())
        .await
        .unwrap();
    let (folder, touched, home) = ask();
    let state = status(folder, touched, home).await.unwrap();

    assert!(
        matches!(committed, GitFile::Committed { .. }),
        "{committed:?}"
    );
    assert_eq!(blamed.head, head);
    assert_eq!(logged.len(), 1);
    assert!(
        matches!(&shown.texts, CommitTexts::Text { after: Some(text), .. } if text == "one\ntwo\n"),
        "{shown:?}"
    );
    assert_eq!(state.checkouts.len(), 1);
    assert_eq!(markers.written(), Vec::<String>::new());
    // Without the daemon's safeguards, git runs them.
    repo.try_git(&["status", "--porcelain"]);
    repo.try_git(&["blame", "HEAD", "--", "notes.txt"]);
    repo.try_git(&["log", "-1"]);
    let ran = markers.written();
    for marker in ["clean", "fsmonitor", "gpg", "textconv"] {
        assert!(
            ran.iter().any(|name| name == marker),
            "{marker} is not live: {ran:?}"
        );
    }
}

#[tokio::test]
async fn a_filter_whose_name_git_cannot_turn_off_refuses_the_checkout() {
    let repo = TestRepo::new();
    let markers = Markers::new(&repo);
    repo.write(".gitattributes", "* filter=a=b\n");
    repo.write("notes.txt", "one\n");
    repo.commit("Start");
    repo.git(&[
        "config",
        "filter.a=b.clean",
        &markers.command("clean", "cat"),
    ]);
    repo.write("notes.txt", "one\ntwo\n");

    let refused = status(repo.root.clone(), Vec::new(), Some(repo.home().to_owned())).await;

    let error = refused.unwrap_err().to_error_body();
    assert_eq!(error.code, ErrorCode::Unsupported);
    assert_eq!(markers.written(), Vec::<String>::new());
}

/// `git diff` writes the index when a file's time changed but not its text, and writing the
/// index runs the `post-index-change` hook.
#[tokio::test]
async fn a_status_writes_no_index_so_runs_no_hook() {
    let repo = TestRepo::new();
    let markers = Markers::new(&repo);
    repo.write("notes.txt", "one\n");
    repo.commit("Start");
    let hook = repo
        .root
        .join(".git")
        .join("hooks")
        .join("post-index-change");
    write_script(&hook, &markers.command("hook", "true"));
    write_later(&repo.root.join("notes.txt"), "one\n");

    let state = status(repo.root.clone(), Vec::new(), Some(repo.home().to_owned()))
        .await
        .unwrap();

    assert_eq!(state.checkouts[0].files, []);
    assert_eq!(markers.written(), Vec::<String>::new());
    repo.try_git(&["diff", "HEAD"]);
    assert_eq!(markers.written(), ["hook"], "the hook is live");
}

/// A partial clone fetches a missing object from its remote, and a fetch runs a program the
/// config names.
#[tokio::test]
async fn a_partial_clone_fetches_no_missing_object() {
    let origin = TestRepo::new();
    let markers = Markers::new(&origin);
    origin.write("notes.txt", "one\n");
    origin.commit("Start");
    origin.git(&["config", "uploadpack.allowFilter", "true"]);
    let clone = origin.partial_clone_beside("clone");
    let upload = markers.command("upload-pack", "false");
    clone.git(&["config", "remote.origin.uploadpack", &upload]);
    let notes = clone.root.join("notes.txt");

    let read = file(
        clone.root.clone(),
        Vec::new(),
        Some(clone.home().to_owned()),
        notes,
    )
    .await;
    let state = status(
        clone.root.clone(),
        Vec::new(),
        Some(clone.home().to_owned()),
    )
    .await;

    assert!(read.is_err(), "the blob is missing: {read:?}");
    assert!(state.is_ok(), "{state:?}");
    assert_eq!(markers.written(), Vec::<String>::new());
    clone.try_git(&["cat-file", "blob", "HEAD:notes.txt"]);
    assert_eq!(markers.written(), ["upload-pack"], "a fetch is live");
}
