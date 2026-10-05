# AGENTS.md

Instructions for AI coding agents working in this repository.

## Always end a run with a commit message

After every run (every completed task), finish your response with a single,
self-contained commit message the user can copy and paste. Follow the style
already used in this repository's history: an imperative subject line, then
scoped bullet points naming the files or modules touched. Do **not** run
`git commit` (or create branches) unless explicitly asked.

## Preparing for publication on crates.io

When the user asks to prepare this crate for publication on crates.io,
perform steps 1–4 below. Documentation is out of scope: do not analyze
documentation coverage, and do not add or change documentation metadata
(the `documentation` field in `Cargo.toml` or `[package.metadata.docs.rs]`);
those are maintained separately.

1. Analyze the **test** coverage (`cargo llvm-cov --all-features`; doctests
   are not measured on stable Rust) and write a report into `Changelog.md`
   under the release's `### Quality` section.
2. Run all tests (`cargo test --all-features`) and report the results in
   `Changelog.md`. When the task says a GROBID server is running, also
   exercise the client against it end to end and include those results.
3. Run and keep green the mandatory checks:
   `cargo fmt --check`, `cargo check --all-targets --all-features`,
   `cargo test --all-features`,
   `cargo clippy --all-targets --all-features -- -D warnings`,
   `cargo doc --no-deps --all-features`, `cargo package --list` and
   `cargo publish --dry-run`. Fix problems instead of silencing them, and
   report release blockers (e.g. a dependency version that is not on
   crates.io yet).
4. Update the package metadata in `Cargo.toml`: version, description,
   keywords, categories, repository and readme. Leave the documentation
   metadata alone (see above).

## Project notes

- This repository is a **library** crate (package `grobid`): a client for
  the GROBID REST API with a strongly typed TEI parser. It has no binaries
  of its own; the BibTeX helpers and the `pdf2bibtex`/`refs2bibtex` tools
  live in the companion crate `grobid-bibtex`.
- MSRV is Rust 1.85 (declared in `Cargo.toml`), edition 2021.
- `Cargo.lock` is intentionally not tracked — it is a library.
- `AGENTS.md` is excluded from the published crate (`exclude` in
  `Cargo.toml`): it lives on GitHub only.
- Before finishing a change, run and keep warning-free:
  `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test`,
  `cargo doc --no-deps`.
- Documentation is enforced via `#![warn(missing_docs)]`; keep `# Errors` /
  `# Panics` sections on public fallible functions up to date.
- Tests that need a GROBID server use the in-process mock server in
  `tests/client.rs`; never require a live server for `cargo test`.
