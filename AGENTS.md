# agents-cockpit

agents-cockpit runs Claude Code agents on a server and lets you steer them from a desktop app.
Read [docs/plan.md](docs/plan.md) for what it does and [docs/architecture.md](docs/architecture.md) for how the code is split, before changing anything.
Read [docs/handoff.md](docs/handoff.md) for where the work stands and how the user works.

## Commands

Run everything inside the dev shell: `nix develop` (stable Rust, nightly rustfmt, clippy, the WebKitGTK libraries).
Use quiet flags so the output stays short.

- Build: `cargo check -q --workspace --all-targets`
- Test: `cargo test -q --workspace`
- Lint: `cargo clippy -q --workspace --all-targets` (the workspace lints and `.cargo/config.toml` apply the standard's clippy settings)
- Format: `cargo fmt` (the root `rustfmt.toml` is the standard's; the dev shell's rustfmt is nightly, as the standard requires)

The review skills write `cargo +nightly fmt`; in this repository, `cargo fmt` in the dev shell is the same thing.

## Working beside other agents

Several agent sessions work on this repository at once.

1. Work in your own git worktree, on your own branch: `git worktree add ../agents-cockpit-<topic> -b feat/<topic> main`.
2. Leave the main checkout on a clean `main`. It is only for merging finished branches and pushing. Merge only once it has no uncommitted changes.
3. A new worktree has its own `target/` folder, so its first build takes a few minutes.

## Rules

- The desktop app's interface follows [crates/cockpit_desktop/AGENTS.md](crates/cockpit_desktop/AGENTS.md): feedback on every control, and animated changes of size, place and presence.
- Rust code follows [docs/standards/rust/](docs/standards/rust/README.md).
  Every crate is application code.
- Review skills: `/review` (correctness), `/review-standard` (the coding standard), `/review-architecture`.
  They were written for another project: where they name its specs or its trust model, use this repository's instead (docs/plan.md, "Security").
- After a feature or a fix, before it is merged: run `/review fix`, then `/review-standard fix`, and commit their fixes on the branch.
- Nothing private goes in this repository: no host names, user names, home paths, emails, tokens, or captured agent output.
  Test fixtures are written by hand with made-up paths, ids and text.
- A prompt never goes to `claude` as a command-line argument: other users of the machine can read arguments.
