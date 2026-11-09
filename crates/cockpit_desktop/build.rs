//! Tells the app which commit it is built from, as `AGENTS_COCKPIT_COMMIT`: the variable when the
//! environment sets it, as the Nix build does, and otherwise git's `HEAD`.

use std::path::Path;
use std::process::Command;

const COMMIT_VAR: &str = "AGENTS_COCKPIT_COMMIT";

fn main() {
    println!("cargo:rerun-if-env-changed={COMMIT_VAR}");
    println!("cargo:rerun-if-changed=build.rs");
    if let Ok(commit) = std::env::var(COMMIT_VAR) {
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
