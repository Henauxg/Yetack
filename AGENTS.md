# Yetack

Yetack runs Claude Code agents on a server and lets you steer them from a desktop app.
Read [README.md](README.md) for what it does and [docs/architecture.md](docs/architecture.md) for how the code is split and why, before changing anything.
[docs/development.md](docs/development.md) says how to build and check the app on each platform, and [docs/todos.md](docs/todos.md) lists what is open.

## How to develop

Several agent sessions work on this repository at once, so each task gets its own worktree and branch.

1. Create a worktree for the task: `git worktree add ../yetack-<topic> -b <branch> main`, where `<branch>` is `feat/<topic>`, `fix/<topic>` or `docs/<topic>`.
   Its first build takes a few minutes, because it has its own `target/` folder.
2. For a new screen or a large change to the interface, make an HTML mockup first: one self-contained file that reuses the app's `style.css`, kept outside the repository, with screenshots.
   Send it to the user, and build only once they have picked one.
3. Build the feature or the fix in the worktree. Run `just check` and `just fmt` in the dev shell, then commit on the branch.
   Each bug fix comes with a test that failed before the fix.
4. Run `/review fix`, then `/review-standard fix`, and commit their fixes on the branch.
   When a change touches both the daemon and the desktop app, run `/review fix` in two agents at once, one per side.
5. If `main` has moved on, merge it into the branch and run the tests there.
6. Once the user agrees, squash-merge the branch from the main checkout: `git merge --squash <branch>`, then commit.
   The commit's subject says what the change does, and its body lists the main points.
7. Fetch `origin`, check that `main` is up to date with `origin/main`, and push `main`.
   Never force-push, and push no tag unless the user asks for a release.
8. Remove the worktree and the branch: `git worktree remove ../yetack-<topic>`, then `git branch -D <branch>`.
   The `-D` is needed because git does not count a squashed branch as merged.

Steps 6 to 8 are the maintainer's. Working for a contributor, stop after step 5 and open a pull request, as [CONTRIBUTING.md](CONTRIBUTING.md) says.

The main checkout stays on a clean `main`. Use it only to merge finished branches and to push.

## Rules

- The user decides and agents propose. Ask before a design change, a protocol change, or anything outside the request, and give your recommendation first.
- The desktop app's interface follows [crates/cockpit_desktop/AGENTS.md](crates/cockpit_desktop/AGENTS.md): feedback on every control, and animated changes of size, place and presence.
- Rust code follows [docs/standards/rust/](docs/standards/rust/README.md).
  Every crate is application code.
- Three skills review a change: `/review` looks for bugs, `/review-standard` checks the coding standard, and `/review-architecture` checks a crate's design.
  The trust model they review against is in docs/architecture.md, "Security".
  They are in `.claude/skills/<name>/SKILL.md`. An agent without skills reads that file and follows it: `/review fix` means the `review` skill with the argument `fix`.
- `docs/standards/rust/` and the three review skills are copies from [rust-standard](https://github.com/Henauxg/rust-standard).
  Make a change there first, then copy it here with `just update-standard <tag, branch or full commit hash>`.
- A commit message has a plain English subject in the imperative and a short body, with no `Co-Authored-By` or other trailer.
- A code comment says only what the code cannot show.
- Nothing private goes in this repository: no host names, user names, home paths, emails, tokens, or captured agent output.
  Test fixtures are written by hand with made-up paths, ids and text.
- A prompt never goes to `claude` as a command-line argument: other users of the machine can read arguments.
