# grobid-rs

A native Rust client for the [GROBID](https://grobid.readthedocs.io/) REST API,
with a strongly typed parser for GROBID TEI documents.

GROBID is a machine learning system for extracting, parsing and re-structuring
raw documents such as PDFs into structured TEI-XML. This crate provides:

- an **asynchronous HTTP client** (`GrobidClient`, built on `reqwest`) for the
  GROBID web services, with automatic retries with exponential backoff when
  the server is busy (HTTP 503/429),
- a **TEI parser** that turns GROBID responses into strongly typed Rust
  structures, exposing the full document hierarchy (sections, paragraphs,
  figures, formulas, footnotes, lists), bibliographic metadata, citations,
  references and optional PDF coordinates.

The client behaviour follows the official
[Python client](https://github.com/grobidOrg/grobid-client-python); the TEI
parser is modelled after the
[Go client](https://github.com/miku/grobidclient), which is itself
modelled after the `grobid-tei-xml` Python package.

## Usage

```rust
use grobid::{GrobidClient, ProcessOptions};

// Inside a `#[tokio::main]` function:
async fn example() -> Result<(), grobid::Error> {
    let client = GrobidClient::new("http://localhost:8070")?;
    assert!(client.ping().await?);

    // Process a PDF and inspect the parsed TEI.
    let document = client
        .process_fulltext_document("paper.pdf", &ProcessOptions::default())
        .await?;
    println!("title: {:?}", document.header.title);
    println!("authors: {}", document.header.authors.len());
    println!("citations: {}", document.citations.len());

    for section in &document.body {
        println!("## {}", section.head.as_deref().unwrap_or("(untitled)"));
        for paragraph in section.paragraphs() {
            println!("{}", paragraph.text);
        }
    }
    Ok(())
}
```

PDFs can also be passed as bytes, avoiding a round-trip through the
filesystem:

```rust
use grobid::{GrobidClient, PdfInput, ProcessOptions};

async fn example(downloaded_bytes: Vec<u8>) -> Result<(), grobid::Error> {
    let client = GrobidClient::new("http://localhost:8070")?;
    let pdf = PdfInput::Data {
        filename: "paper.pdf".to_string(),
        data: downloaded_bytes,
    };
    let document = client
        .process_fulltext_document(pdf, &ProcessOptions::default())
        .await?;
    Ok(())
}
```

### Services

| Method | GROBID service | Returns |
| --- | --- | --- |
| `process_fulltext_document` | `/api/processFulltextDocument` | `Document` |
| `process_header_document` | `/api/processHeaderDocument` | `Document` |
| `process_references` | `/api/processReferences` | `Vec<Citation>` |
| `process_citation` | `/api/processCitation` | `Option<Citation>` |
| `process_citation_list` | `/api/processCitationList` | `Vec<Citation>` |
| `process_pdf_raw` | any multipart service | raw TEI/response body |
| `ping` | `/api/isalive` | `bool` |
| `version` | `/api/version` | `String` |

### Options

`ProcessOptions` controls the request parameters; its defaults follow the
GROBID server defaults and the official Python client (header consolidation
on, citation consolidation off):

```rust
use grobid::{CoordinateElement, GrobidClient, ProcessOptions};

async fn example() -> Result<(), grobid::Error> {
    let client = GrobidClient::new("http://localhost:8070")?;
    let options = ProcessOptions {
        generate_ids: true,
        include_raw_citations: true,
        tei_coordinates: CoordinateElement::COMMON.to_vec(),
        segment_sentences: true,
        ..Default::default()
    };
    let document = client
        .process_fulltext_document("paper.pdf", &options)
        .await?;
    // Coordinates of figures, references, ... in the original PDF:
    for figure in document.body[0].figures() {
        println!("{:?} at {:?}", figure.head, figure.coords);
    }
    Ok(())
}
```

### Parsing TEI without a server

The parser works on TEI XML obtained elsewhere, e.g. written to disk by a
batch process:

```rust
use grobid::tei::parse_document;

fn example() -> Result<(), Box<dyn std::error::Error>> {
    let xml = std::fs::read_to_string("output.tei.xml")?;
    let document = parse_document(&xml)?;
    println!("{:?}", document.header.title);
    Ok(())
}
```

The parser is namespace-agnostic: it accepts both full TEI documents in the
TEI namespace and bare `<biblStruct>` fragments as returned by
`/api/processCitation`.

### Serialization

All parsed types implement `serde::Serialize`/`Deserialize`, so a document
can be converted to JSON (or any other `serde` format) with e.g. `serde_json`:

```rust
fn example(xml: &str) -> Result<(), Box<dyn std::error::Error>> {
    let document = grobid::tei::parse_document(xml)?;
    let json = serde_json::to_string_pretty(&document)?;
    println!("{json}");
    Ok(())
}
```

## Coordinates

When `tei_coordinates` is requested, GROBID adds a `@coords` attribute to the
selected structures and a `<facsimile>` section with the page dimensions.
Both are parsed:

- `Coords`/`BoundingBox` on sections, paragraphs, sentences, figures,
  formulas, notes, citations, authors, ...
- `document.facsimile: Vec<Surface>` with the width/height of every page.

The coordinate system is the native PDF one: origin at the upper-left corner
of a page, the y-axis extending downward, values in abstract PDF units, and
the first page numbered 1. See the
[GROBID documentation](https://grobid.readthedocs.io/en/latest/Coordinates-in-PDF/)
for details.

## Examples

### pdf2bibtex

The `pdf2bibtex` example processes all PDFs in a directory and writes the
extracted bibliographic metadata as a BibTeX file, one entry per document
(its header):

```sh
cargo run --release --example pdf2bibtex -- ~/papers -s http://localhost:8070
```

### refs2bibtex

The `refs2bibtex` example extracts the bibliographic *references* of all PDFs
in a directory (via `/api/processReferences`) and writes one BibTeX entry per
reference:

```sh
cargo run --release --example refs2bibtex -- ~/papers -s http://localhost:8070
```

Both examples discover PDFs recursively and process them with a bounded
number of concurrent requests (`-w`, default 4); per-document failures are
reported on stderr and skipped, and an unresponsive server is detected by a
liveness probe with bounded retries and a clear error message. Entry types
(`@article`, `@incollection`, `@techreport`, `@book`, `@misc`) and keys
(first author surname + year, deduplicated) are derived from the parsed
metadata via the `grobid::bibtex` helpers, which render records as typed
BibLaTeX entries using the [`biblatex`](https://crates.io/crates/biblatex)
crate (proper escaping, typed person lists and dates). `refs2bibtex` skips
empty parse results and drops references with a duplicate DOI, and supports
reference consolidation against CrossRef with `-c/--consolidate`. Run either
example with `--help` for all options.

## The GROBID server

This crate is a client only: all PDF parsing happens in a running
[GROBID](https://grobid.readthedocs.io/) server, which is reached over its
REST API. GROBID is a machine learning system that turns PDFs of scholarly
articles into structured TEI XML — header metadata, body structure,
references and optional PDF coordinates. The server preloads its models at
startup and listens on port `8070` by default.

The quickest way to get a server running is Docker, which is also the only
supported option on Windows (GROBID itself is developed and tested on Linux
and macOS). On `arm64` hosts the images run emulated and are considerably
slower:

| Platform | Installation instructions |
| --- | --- |
| Docker (any platform, recommended) | [Run with Docker](https://grobid.readthedocs.io/en/latest/Grobid-docker/) |
| Linux | [Build from source](https://grobid.readthedocs.io/en/latest/Install-Grobid/) (JDK 21, then `./gradlew run`) |
| macOS | [Build from source](https://grobid.readthedocs.io/en/latest/Install-Grobid/) (includes Homebrew JDK instructions) |
| Windows (native) | Not supported by GROBID; use Docker or WSL instead, see [Windows related issues](https://grobid.readthedocs.io/en/latest/Frequently-asked-questions/#windows-related-issues) |

To start a server with Docker — the `-crf` image is small (about 500 MB)
and fast, the `-full` image (about 8 GB, GPU recommended) adds Deep Learning
models that improve reference parsing:

```sh
# CRF models only
docker run --rm --init --ulimit core=0 -p 8070:8070 grobid/grobid:0.9.1-crf

# with Deep Learning models (add --gpus all on Linux to use a GPU)
docker run --rm --init --ulimit core=0 -p 8070:8070 grobid/grobid:0.9.1-full
```

Once the container is up, `http://localhost:8070/api/isalive` returns `true`
and the examples above work as shown. See the GROBID documentation for the
[quick start](https://grobid.readthedocs.io/en/latest/getting_started/), the
[REST API reference](https://grobid.readthedocs.io/en/latest/Grobid-service/)
and the
[troubleshooting guide](https://grobid.readthedocs.io/en/latest/Frequently-asked-questions/).

## Requirements

- Rust 1.85+
- A running GROBID server, see *The GROBID server* above

## License

MIT, see [LICENSE](https://github.com/schmettow/grobid-rs/blob/main/LICENSE).
