# AGENTS.md

Instructions for AI coding agents working in this repository.

## Always end a run with a commit message

After every run (every completed task), finish your response with a single,
self-contained commit message the user can copy and paste. Follow the style
already used in this repository's history: an imperative subject line, then
scoped bullet points naming the files or modules touched. Do **not** run
`git commit` (or create branches) unless explicitly asked.

## Project notes

- This repository is a **library** crate (package `grobid`): a client for
  the GROBID REST API with a strongly typed TEI parser. It has no binaries
  of its own; the `examples/` are reference implementations.
- MSRV is Rust 1.85 (declared in `Cargo.toml`), edition 2021.
- `Cargo.lock` is intentionally not tracked — it is a library.
- `AGENTS.md` is excluded from the published crate (`exclude` in
  `Cargo.toml`): it lives on GitHub only.
- Before finishing a change, run and keep warning-free:
  `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test`,
  `cargo test --examples`, `cargo doc --no-deps`.
- Documentation is enforced via `#![warn(missing_docs)]`; keep `# Errors` /
  `# Panics` sections on public fallible functions up to date.
- Tests that need a GROBID server use the in-process mock server in
  `tests/client.rs`; never require a live server for `cargo test`.
