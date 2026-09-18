//! Parsing of GROBID TEI-XML into strongly typed structures.
//!
//! See the crate-level documentation for an overview.

mod model;
mod parse;

pub use model::{
    Address, Affiliation, Author, Biblio, Block, BoundingBox, Citation, Coords, Div, Document,
    Figure, Formula, Header, List, MarkerRef, Note, Paragraph, RefKind, Sentence, Surface,
};
pub use parse::{parse_citation, parse_citation_list, parse_document};
