# Roadmap

Directions for `grobid-rs` beyond the current release. The list is a priority
sketch, not a schedule: each item names its motivation and the trade-offs to
weigh before it is started.

## 1. Reference completion: a maintained OpenAlex client — or our own

The `openalex` feature currently completes references against OpenAlex through
a small, tolerant in-house model in `src/openalex.rs` (requests via the async
`reqwest` client the crate already uses). This exists because the `openalex`
crate (0.2.2) predates current OpenAlex API schema changes and its blocking,
URL-hardcoded HTTP helpers cannot be tested against a mock or a mirror.

Revisit the decision when the feature grows beyond a thin completion tier:

- evaluate maintained async clients (`papers-openalex`, `openalex-rs`, forks
  of `openalex`) for coverage, maintenance activity, license, MSRV and
  dependency weight — in particular whether they force a second `reqwest`
  major version, which this crate deliberately moved away from;
- weigh them against the cost of maintaining our own model. Owning the model
  is what makes the current parser tolerant of unknown, missing and `null`
  fields; a third-party model may be stricter and break on API drift;
- whatever the outcome, keep the public `openalex::Completer` API stable and
  swap only the internals.

## 2. Grow the CLI examples before splitting them out

`pdf2bibtex` and `refs2bibtex` are reference implementations in `examples/`.
Develop them there first — more options, better progress output, clearer
error reporting, additional heuristics — so that the library API is shaped by
real command-line needs. Only then move them into their own crate; until
then, every new option is a cheap experiment.

## 3. Ragrig connector

Write a connector for the Ragrig crate, so documents processed by GROBID —
headers, body structure, completed references — can flow directly into Ragrig
pipelines.

## 4. More output adapters

Provide output adapters beyond BibTeX, e.g. XML and APA. A common output
format (for instance via the `commonmeta` crate) would avoid implementing and
maintaining every target format separately; the order of these adapters
depends on whether `commonmeta` is a suitable dependency.
