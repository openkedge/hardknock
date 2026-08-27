# Contributing

Hardknock currently implements the execution substrate for V0.1, not the Experience Engine. Read the [architecture](docs/architecture.md) and [milestone plan](docs/roadmap.md) before extending the interfaces.

## Development

Use Linux or macOS, Git, a C compiler for bundled SQLite, and stable Rust with rustfmt and clippy. `rust-toolchain.toml` selects the toolchain components; `Cargo.lock` pins dependencies.

```bash
cargo build --locked
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
```

Tests create temporary Git repositories and private data directories. They exercise real processes, SQLite, worktrees, process failure, deadlines, and signal cleanup. They do not require an LLM, paid API, external database, Node, npm, or pnpm. Dependency downloads are needed for the first Cargo build; the tests themselves are offline.

## Changes and evidence

Keep changes small and scoped to a milestone. Preserve the distinction between Reality, execution, Experience, and Lesson. Add tests for behavioral changes, especially failure and cleanup paths. Never turn a process exit code into a task-success assertion without an evaluator.

Add new migrations instead of editing an already released schema. Keep stdout machine readable in JSON mode, and keep internal logs on stderr. Do not log inherited environment variables or add production credentials to fixtures. Document any new execution authority or isolation limitation.

Before proposing a learning feature, specify the control, expected observation, scope, and evidence that would contradict the hypothesis. Do not add a vector store or distributed service to solve a problem deterministic local code can address.

## License status

The project license is still **TBD**. This contribution guide does not select a license or add a license grant. Resolve licensing with the maintainers before distributing a release.
