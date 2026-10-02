# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Optional second-tier reference completion against OpenAlex behind the
  `openalex` cargo feature: `openalex::Completer` looks a reference up by
  DOI (or, when none was parsed, by title) and fills in missing authors,
  journal, volume, pages, DOI, PubMed ID and URL without overwriting parsed
  values. Requests are made with async `reqwest` against a small, tolerant
  in-house model of the OpenAlex response; no external OpenAlex client is
  required.
- The matching heuristics (title normalization, year and author checks,
  candidate ranking) are pinned by an extensive unit test suite. Known
  weaknesses are documented as `#[ignore]`d tests that assert the desired
  behavior and fail today, so the backlog is measurable by the number of
  ignored tests.
- `Roadmap.md`: the next steps beyond this release (OpenAlex client choice,
  CLI examples, Ragrig connector, output adapters).
- `refs2bibtex` and `pdf2bibtex`: with `--openalex` (requires building the
  example with `--features openalex`), extracted references respectively
  document headers are completed against OpenAlex after parsing.

### Fixed

- Reference completion no longer fails on titles that contain OpenAlex
  filter syntax: commas, pipes (`|`), exclamation marks, quotes and wildcards
  (`*`, `?`) are replaced by spaces before the title is sent as a search
  term, so titles such as `Sexual selection, sensory systems and sensory
  exploitation` or a title with a stray trailing `*` complete instead of
  being rejected with HTTP 400 (or silently turned into a broader query).

## [0.2.0] - 2026-10-02

### Added

- `bibtex::format_entry_with_file()`: renders a bibliographic record as a
  BibTeX entry with a verbatim `file` field naming a local document, e.g.
  for a reference manager to open the PDF the metadata came from.
- `pdf2bibtex`: `-r`/`--rename` renames each processed PDF after its
  metadata has been extracted to `Author_Year_<first 10 title words>`,
  keeping the original file extension. Missing metadata parts are omitted,
  non-ASCII characters are dropped (as for citation keys), and name
  collisions get a `-2`, `-3`, ... suffix; a PDF without any usable author,
  year or title keeps its name.
- `pdf2bibtex`: `-l`/`--link` adds a `file` field with the path of the PDF
  to every entry. Combined with `-r`/`--rename` the field points at the
  renamed file; a failed rename keeps the original path, so entries always
  refer to an existing file. Paths are recorded as passed on the command
  line, so relative input paths stay relative.

### Changed

- Updated the `roxmltree` dependency to 0.21.
- README: the `pdf2bibtex` section documents the new `-r`/`--rename` and
  `-l`/`--link` options.

[0.2.0]: https://github.com/schmettow/grobid-rs/compare/v0.1.0...v0.2.0
