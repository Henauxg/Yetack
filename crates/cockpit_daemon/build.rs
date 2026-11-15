//! Tells the daemon which commit it is built from, as `YETACK_COMMIT`: from
//! `YETACK_SOURCE_COMMIT` when the environment sets it, as the Nix and release builds do,
//! and otherwise from git's `HEAD`. Each program that runs the daemon, `yetack` and the
//! app, is built with it from the same tree, so this is their commit too.

use std::path::Path;
use std::process::Command;

/// Not the variable this script writes: `cargo run` puts that one in the program's environment,
/// and the agents the program starts would build with their parent's commit.
const SOURCE_VAR: &str = "YETACK_SOURCE_COMMIT";
const COMMIT_VAR: &str = "YETACK_COMMIT";

fn main() {
    println!("cargo:rerun-if-env-changed={SOURCE_VAR}");
    println!("cargo:rerun-if-changed=build.rs");
    if let Ok(commit) = std::env::var(SOURCE_VAR) {
        println!("cargo:rustc-env={COMMIT_VAR}={commit}");
        return;
    }
    if let Some(commit) = git(&["rev-parse", "HEAD"]) {
        println!("cargo:rustc-env={COMMIT_VAR}={commit}");
    }
    // These paths work in a linked worktree too: git resolves them to the right folders.
    if let Some(head) = git_path("HEAD") {
        watch(Path::new(&head));
    }
    if let Some(packed_refs) = git_path("packed-refs").filter(|path| Path::new(path).exists()) {
        watch(Path::new(&packed_refs));
    }
    if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git_path(&reference)
    {
        watch(nearest_existing(Path::new(&path)));
    }
}

fn git(arguments: &[&str]) -> Option<String> {
    let output = Command::new("git").args(arguments).output().ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (output.status.success() && !text.is_empty()).then(|| text.to_owned())
}

fn git_path(name: &str) -> Option<String> {
    git(&["rev-parse", "--git-path", name])
}

fn watch(path: &Path) {
    println!("cargo:rerun-if-changed={}", path.display());
}

/// A branch's ref has no file of its own once it is packed, and cargo always reruns for a path
/// that does not exist: the folder that would hold it shows the next commit.
fn nearest_existing(mut path: &Path) -> &Path {
    while !path.exists() {
        match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => path = parent,
            Some(_) | None => break,
        }
    }
    path
}
