---
name: review
description: Review code for correctness, for a pull request, a branch, the working tree or a path. Does the code work, does it do what it must, does it handle edge cases and errors, can a player, a pack or a network peer make it misbehave. With the `fix` argument, fix the bugs found, each with a test, in passes, until a pass finds none. Use when asked to review changes for bugs or security holes, check that code works, or find unhandled cases. Style and the coding standard are the review-standard skill's job.
---

# Review code for correctness

This skill checks that code works: it does what it must, on every input it can
get, it fails cleanly where it cannot, and nobody outside the GM's machine can
make it misbehave. With `fix` it fixes the bugs it finds.

Style and the coding standard are out of scope: `/review-standard` checks them.
A rule of the standard still matters here when breaking it causes a bug, such
as a panic on input a user can supply.

## Arguments

`/review [fix] [target] [intent]`

- `fix`: fix the bugs found, in passes (below). Without it, only report.
- `target`, what to review:
  - nothing: the current branch's changes, both committed since it left the
    default branch and uncommitted, untracked files included;
  - a pull request number or URL: its changes against its base;
  - a branch or a commit range: `main..feature`;
  - a path (a file, a directory, a crate): all of its code, not only changes.
- `intent`: any other text says what the code must do. It comes before what
  the skill finds in other sources (step 1).

## What is judged

- Code in any language. Rust gets the cargo commands below. For other code,
  use what the repository provides to build and test it.
- For changes: the changed code, and what the change affects outside it. That
  includes the callers of a changed function, the users of a changed type, and
  the data the code reads or writes: saved campaigns, the database, packs,
  network messages. A bug the change causes elsewhere is a finding of the
  change.
- A bug in code the change neither touches nor depends on is pre-existing. It
  goes in its own list, and is never fixed unless the user asks.

## 1. Find what the code must do

A review judges code against its intent. Before reading for bugs, write down
what the code must do, as a short list of behaviors, each with its source:

- the intent given when invoking the skill;
- the pull request's description, and the issues it links;
- the commit messages;
- the specs in `docs/` that the code implements, such as `DATA_PACKS.md`,
  `CAMPAIGN.md` and `NETWORK.md`;
- doc comments and the existing tests;
- for a path: the module's documentation and its callers.

When two sources disagree (the code and the spec, the pull request and the
spec), the disagreement is a finding that needs a decision. When nothing says
what the code must do, state the intent you assume, and mark the findings that
depend on it.

## 2. Collect what to review

- Nothing given: find the default branch (`git remote show origin`, else
  `main`), take `git merge-base HEAD origin/<default>`, and review
  `git diff <merge-base>` plus untracked files.
- A pull request: fetch its head (`git fetch origin pull/<n>/head`) and diff
  against its base, or use the host's pull request tools.
- A branch or a range: `git diff <range>`.
- A path: every file under it.

Read each changed file in full, not only the diff. Find the code that depends
on what changed: search for the names of changed functions, types and fields.
List the crates touched (the nearest `Cargo.toml` above each file), and the
workspace crates that depend on them.

## 3. Run what can run

Use quiet flags (the repository's quiet-compile rule). For each crate listed in
step 2:

1. **Build:** `cargo check -q -p <crate> --all-targets`.
2. **Tests:** `cargo test -q -p <crate>`. A failing test is a finding, unless
   it fails the same way on the base. Then it is pre-existing.
3. **Lints:** `cargo clippy -q -p <crate> --all-targets`. Keep the diagnostics
   of the `correctness` and `suspicious` groups, and any other that points at a
   bug.

Then run the code itself when there is a way: a tool on a real input (the pack
checker on the reference packs), or the app through its `justfile` recipes. A
run shows more than reading.

When a step cannot run (a missing tool, a build already broken on the base),
say so in the report.

## 4. Read for bugs

Follow the data. For each changed function, take each kind of input it can get
and trace it to the output. Check at least:

- **Logic:** an inverted or wrong condition, the wrong operator or variable, an
  off-by-one or a wrong range bound, an early `return` or `continue` that skips
  work, a `match` arm or `if let` that silently drops a case, a `_` arm that
  swallows variants added later, shadowing.
- **Edge cases:** empty, one element, many, duplicates; zero, negative, the
  maximum; integer overflow (it wraps in release builds), division by zero,
  the rounding direction (rules often round down), casts that truncate or
  change the sign; text with Unicode, case and whitespace; paths (separators,
  case on Windows and macOS, a missing or unreadable file); an absent optional
  value; a reference to something that does not exist; cycles, such as packs
  depending on each other or derived values reading each other.
- **Errors:** every error reaches someone who can act on it. No error is
  swallowed (`let _ =`, `.ok()`, `unwrap_or_default()` hiding a failure). The
  message names what failed and where. After a failure the state is
  consistent: no half-applied change, no partly written file.
- **State and order:** invariants hold on every path, the error paths
  included; no mutation while iterating; no order that depends on `HashMap`
  iteration or on timing where the result must be deterministic (the engine
  replicates state over the network, so the same input must give the same
  result on every machine); no event handled twice or never; initialization
  in the right order; Bevy systems that rely on each other's writes have an
  explicit order.
- **Concurrency and async:** shared state without synchronization, a lock held
  across `.await`, locks taken in different orders, a task dropped or never
  awaited, a cancellation that leaves partial state.
- **Compatibility:** a changed function still fits every caller; a changed
  type still reads the data saved before (files, database, its migrations); a
  changed message still matches what the other side sends.
- **Resources:** files, connections and tasks released on every path; no cache
  or queue that grows without bound.
- **The intent:** every behavior listed in step 1 is implemented, the cases
  the spec names in particular, and nothing the spec forbids is allowed.
- **Tests:** each behavior of step 1, and each edge case above that applies,
  has a test. A test asserts a result, not only that the code runs. A test
  that passes for the wrong reason (it asserts nothing, it tests a mock, its
  input never reaches the branch) counts as missing.

Performance is in scope only where it breaks behavior: a hang, a timeout,
memory that grows with time, a quadratic loop on input sizes the app sees.

## 5. Check security

Read the code again as someone who wants to break it. The trust model is in
`docs/NETWORK.md` ("Trust"): the GM's machine is the authority, and each
player's process holds only what its seat may see. Treat as hostile:

- a player's client, and every message it sends;
- a network peer, including one that is not part of the table;
- a data pack: its JSON, its code and its files;
- a file from outside: a campaign package, an import, an image.

Check at least:

- **Hidden information:** no unrevealed GM data and no other character's data
  enters a player's process. Look at what is replicated, and also at the
  messages, logs, error texts and offers sent to a client. A value derived
  from a hidden one can leak it, and so can a count, or an id that exists only
  when something hidden exists.
- **Authority:** the GM checks every request from a player and trusts none of
  it: this seat may act on this entity, the decision is still open, the inputs
  type-check, the conditions still hold. A check made only on the client is no
  check. A result the GM must compute is never taken from a client's message.
  An old or repeated request is dropped.
- **Identity:** who sent a message comes from the connection (the peer's key),
  never from a field the sender fills in.
- **Input from outside:** malformed or hostile input gives an error, never a
  panic, a hang or unbounded memory. Sizes and counts read from input are
  bounded before anything is allocated. Nesting and recursion have a depth
  limit. One request cannot cost the GM unbounded work.
- **Packs:** a pack does only what the spec grants it. Pack code (WASM,
  scripts) runs with limits on time and memory, and reaches the host only
  through the API. A path in a pack stays inside the pack: no `..`, no
  absolute path, no symlink leading out. A pack cannot read or change what the
  spec keeps from it.
- **Files and paths:** a name from outside never builds a path that leaves its
  folder. Extracting an archive checks each entry's path. A write never
  replaces a file it was not meant to.
- **Database:** values go into queries as parameters (`sqlx` macros,
  `.bind`), never through `format!`.
- **Secrets:** keys, tickets and tokens never appear in logs or error
  messages, are never committed, and are never sent to another peer.
- **Randomness and cryptography:** randomness that protects something (a key,
  a token) comes from a cryptographic generator. Cryptography comes from
  established crates, never written here.
- **Unsafe and foreign code:** at an `unsafe` block or an FFI boundary (the
  Lua library, the WASM host), the `SAFETY:` reasoning holds for every input
  that can reach it.
- **Dependencies:** a new dependency is maintained, comes from a known source,
  and enables only the features it needs. Run `cargo audit` or
  `cargo deny check advisories` when one is installed. Otherwise say that
  advisories were not checked.

## 6. Verify each finding

A finding states a concrete failure: this input or this state, then this wrong
result, crash or lost data. Before reporting one:

1. Try to prove it wrong. Look for a check elsewhere, a caller that never
   passes that input, a type that rules it out. If one exists, drop the
   finding.
2. Reproduce it where that is cheap: a unit test or a run that fails. A
   reproduced finding is **confirmed**. A finding shown by reading only is
   **likely**.
3. Drop a worry that does not become a concrete scenario.

In review mode, reproduction tests do not stay in the code: remove them after
running them, and quote them in the report so that fix mode or the user can
reuse them.

## 7. Report

One entry per finding, ordered by severity, then by file:

```
high  crates/tc_core/src/rolls.rs:88  confirmed
  A roll of 1 with a modifier of -3 totals 254: the total is a `u8` and wraps.
  Expected: 0, the lowest total a roll can give.
  Fix: add in `i32`, clamp at 0, then convert.

high  crates/tc_net/src/requests.rs:41  likely  security
  A player can move any token: the handler checks that the token exists, not
  that the requesting seat controls it.
  Expected: the GM drops a move request for a token the seat does not control.
  Fix: check the seat's control of the token before applying the move.
```

Severity:

- **high:** a wrong result, a crash, a hang or lost data in normal use, or on
  input from outside;
- **medium:** a wrong result on an edge case a user can reach;
- **low:** an edge case unlikely in practice, or a missing test for a behavior
  that works.

Mark a security finding with `security`, and rate it by who can use it: high
when a player, a pack, a peer or a file from outside can, in normal use.

After the findings:

- the findings that need a decision (below);
- the pre-existing bugs;
- what was checked: the intent list with its sources, the commands run and
  their results, the files read.

When nothing is found, say so, and say what was checked.

## Fix mode: passes

With `fix`, repeat:

1. **Review** the current state of the target (steps 3 to 6).
2. **Stop** if the pass finds nothing to fix: the code is correct as far as
   the review can tell.
3. **Fix** each finding that needs no decision, the most severe first:
   1. write a test that fails because of the bug, and see it fail (for a
      missing test, the test is the fix);
   2. make the smallest change that fixes the bug, and change no behavior the
      intent does not ask to change;
   3. see the test pass.

   When no test can reproduce the bug (timing, the UI), say how the fix was
   checked instead. For a security finding with two possible fixes, take the
   safer one. Rust fixes follow the coding standard
   (`docs/standards/rust/`).
4. **Verify:** build, and run the tests of the touched crates and of the
   crates that depend on them. Format what you changed as `/review-standard`
   does. If something broke, fix it before the next pass.
5. **Go on** to the next pass, which also reviews the fixes themselves.

Stop after five passes, or when a finding comes back after a fix, and report
what is left and why. The tests written for fixes stay in the code. Do not
commit: the caller decides.

## Findings that need a decision

Report these, and do not fix them:

- the intent is unclear, or the code and the spec disagree: which one is
  right?
- the fix changes behavior that other code, saved data or users rely on;
- the fix needs a design change, a new dependency, or a change to a library
  crate's public API;
- an edge case the spec does not settle, such as what an empty dice pool
  rolls: propose an answer;
- a security fix that removes or limits a feature, or needs a limit whose
  value the spec does not set (a message size, a pack's memory): propose
  one.

At the end, ask the user about each of them, with a recommendation.

## Final report in fix mode

- the passes run, and in each the fixes made: file and line, the bug, the test
  that covers it;
- what is left, and why;
- the commands run and their results.
