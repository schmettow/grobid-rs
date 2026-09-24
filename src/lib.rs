//! # grobid
//!
//! A native Rust client for the [GROBID](https://grobid.readthedocs.io/)
//! REST API, with a strongly typed parser for GROBID TEI documents.
//!
//! GROBID is a machine learning library for extracting, parsing and
//! re-structuring raw documents such as PDFs into structured TEI-XML. This
//! crate provides:
//!
//! * an asynchronous HTTP client ([`GrobidClient`]) for the GROBID web
//!   services, with busy-server retries,
//! * a TEI parser that turns GROBID responses into strongly typed Rust
//!   structures ([`Document`], [`Biblio`], [`Citation`], ...), exposing the
//!   full document hierarchy, bibliographic metadata, references and
//!   optional PDF coordinates.
//!
//! ## Example
//!
//! ```no_run
//! use grobid::{GrobidClient, ProcessOptions};
//!
//! # async fn run() -> Result<(), grobid::Error> {
//! let client = GrobidClient::new("http://localhost:8070")?;
//! assert!(client.ping().await?);
//!
//! // Process a PDF and inspect the parsed TEI.
//! let document = client
//!     .process_fulltext_document("paper.pdf", &ProcessOptions::default())
//!     .await?;
//! println!("title: {:?}", document.header.title);
//! println!("authors: {}", document.header.authors.len());
//! println!("citations: {}", document.citations.len());
//!
//! for section in &document.body {
//!     println!("## {}", section.head.as_deref().unwrap_or("(untitled)"));
//!     for paragraph in section.paragraphs() {
//!         println!("{}", paragraph.text);
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## Parsing TEI without a server
//!
//! The parser works on TEI XML obtained elsewhere, e.g. written to disk by
//! a batch process:
//!
//! ```
//! use grobid::tei::parse_document;
//!
//! let xml = r#"<TEI xmlns="http://www.tei-c.org/ns/1.0">
//!     <teiHeader>
//!         <encodingDesc><appInfo>
//!             <application version="0.8.1" when="2024-01-01T00:00+0000"/>
//!         </appInfo></encodingDesc>
//!         <fileDesc>
//!             <titleStmt><title level="a" type="main">Example</title></titleStmt>
//!             <publicationStmt><publisher/></publicationStmt>
//!             <sourceDesc><biblStruct/></sourceDesc>
//!         </fileDesc>
//!     </teiHeader>
//!     <text/>
//! </TEI>"#;
//!
//! let document = parse_document(xml)?;
//! assert_eq!(document.header.title.as_deref(), Some("Example"));
//! # Ok::<(), grobid::Error>(())
//! ```
//!
//! The full README is included below.
//!
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]

pub mod bibtex;
mod client;
mod error;
pub mod tei;

pub use client::{
    CitationConsolidation, CoordinateElement, GrobidClient, GrobidClientBuilder,
    HeaderConsolidation, PdfInput, ProcessOptions, RetryPolicy, DEFAULT_GROBID_URL,
    SERVICE_CITATION, SERVICE_CITATION_LIST, SERVICE_FULLTEXT, SERVICE_HEADER, SERVICE_REFERENCES,
};
pub use error::{Error, InvalidDocument};
pub use tei::{
    parse_citation, parse_citation_list, parse_document, Address, Affiliation, Author, Biblio,
    Block, BoundingBox, Citation, Coords, Div, Document, Figure, Formula, Header, List, MarkerRef,
    Note, Paragraph, RefKind, Sentence, Surface,
};
