# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
