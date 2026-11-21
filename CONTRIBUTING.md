# Contributing

Read [AGENTS.md](AGENTS.md) first: it says how to develop, and lists the coding standard and the rules every change follows. [docs/development.md](docs/development.md) lists the commands.

## Pull requests

1. Fork the repository, and branch from `main`: `feat/<topic>`, `fix/<topic>` or `docs/<topic>`.
2. Follow steps 1 to 5 of "How to develop" in [AGENTS.md](AGENTS.md). Steps 6 to 8, the squash-merge into `main` and the push, are the maintainer's.
3. Open a pull request against `main`. CI builds and tests it on Linux and Windows.
4. Write its title as a commit subject: plain English, in the imperative, saying what the change does. Its description lists the main points. The maintainer squash-merges it, so the title and the description become the commit on `main`.

For a new screen or a large change to the interface, open an issue with the mockup first, and build once the maintainer has picked one.

## License of contributions

Yetack is licensed under the Mozilla Public License 2.0 ([LICENSE](LICENSE)).

By sending a contribution, you agree that it is licensed under the MPL-2.0, and also under the MIT license or the Apache License 2.0, at the maintainer's choice. This lets the project move to MIT or Apache-2.0 later without asking each contributor again.
