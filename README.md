# agents-cockpit

Run several Claude Code agents on a server, and watch and steer them from a desktop app: their conversations, tool calls, approvals, questions and plans.

- The agents run in a daemon on the server, so they keep working when the app disconnects.
- The app reaches the daemon over SSH, or directly on the same machine. The daemon opens no network port.
- Claude Code runs unmodified, in its JSON mode, signed in with your own account.

Status: early development. See [docs/plan.md](docs/plan.md) and [docs/architecture.md](docs/architecture.md).

## Development

```sh
nix develop
cargo test -q --workspace
```
