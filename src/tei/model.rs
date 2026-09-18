//! Strongly typed structures for GROBID TEI documents.
//!
//! The types in this module mirror the content model of the TEI XML that
//! GROBID produces (see the GROBID documentation at
//! <https://grobid.readthedocs.io/>). All types are plain data containers
//! with `serde` support, so a parsed [`Document`] can be serialized to JSON
//! or any other `serde`-compatible format.

use serde::{Deserialize, Serialize};

/// A rectangular region on a PDF page.
///
/// GROBID uses the native PDF coordinate system: the origin is at the
/// *upper left* corner of a page, the x-axis extends to the right and the
/// y-axis extends *downward*. Values are in abstract PDF units, and the
/// first page has index 1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BoundingBox {
    /// Page number, starting at 1.
    pub page: u32,
    /// X coordinate of the upper-left corner.
    pub x: f64,
    /// Y coordinate of the upper-left corner.
    pub y: f64,
    /// Width of the box.
    pub width: f64,
    /// Height of the box.
    pub height: f64,
}

/// The coordinates of a TEI structure in the original PDF.
///
/// A structure may span several bounding boxes, e.g. when a reference is
/// broken over several lines. Coordinates are only present when the
/// `teiCoordinates` parameter was requested from GROBID.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Coords {
    /// The bounding boxes that make up the structure's area.
    pub boxes: Vec<BoundingBox>,
}

impl Coords {
    /// Parse a GROBID `@coords` attribute value.
    ///
    /// The compact notation consists of bounding boxes separated by `;`,
    /// each box being `page,x,y,width,height`:
    ///
    /// ```text
    /// "1,53.80,194.57,58.71,9.29;1,53.80,202.57,58.71,9.29"
    /// ```
    pub fn parse(value: &str) -> Result<Self, crate::Error> {
        let mut boxes = Vec::new();
        for part in value.split(';') {
            let fields: Vec<&str> = part.split(',').map(str::trim).collect();
            let [page, x, y, width, height] = fields[..] else {
                return Err(crate::Error::InvalidCoords {
                    value: value.to_string(),
                });
            };
            let parse = |s: &str| {
                s.parse::<f64>().map_err(|_| crate::Error::InvalidCoords {
                    value: value.to_string(),
                })
            };
            let page = page
                .parse::<u32>()
                .map_err(|_| crate::Error::InvalidCoords {
                    value: value.to_string(),
                })?;
            boxes.push(BoundingBox {
                page,
                x: parse(x)?,
                y: parse(y)?,
                width: parse(width)?,
                height: parse(height)?,
            });
        }
        Ok(Coords { boxes })
    }

    /// Returns true if there are no bounding boxes.
    pub fn is_empty(&self) -> bool {
        self.boxes.is_empty()
    }
}

/// The dimension of a page, taken from the `<facsimile>` section of a TEI
/// document. Page dimensions are included when coordinates were requested.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Surface {
    /// Page number, starting at 1.
    pub page: u32,
    /// Page width in PDF units.
    pub width: f64,
    /// Page height in PDF units.
    pub height: f64,
}

/// A person (author or editor), parsed from a TEI `<persName>` element.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Author {
    /// The full name, as it appears in the document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_name: Option<String>,
    /// First name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub given_name: Option<String>,
    /// Middle name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub middle_name: Option<String>,
    /// Last name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surname: Option<String>,
    /// Email address, when present in the header.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// ORCID identifier, when present in the header.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orcid: Option<String>,
    /// Affiliation, when present in the header.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affiliation: Option<Affiliation>,
    /// Coordinates in the original PDF, when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coords: Option<Coords>,
}

impl Author {
    /// Returns true if nothing is known about this author.
    pub fn is_empty(&self) -> bool {
        self.full_name.is_none()
            && self.given_name.is_none()
            && self.middle_name.is_none()
            && self.surname.is_none()
            && self.email.is_none()
            && self.orcid.is_none()
            && self.affiliation.as_ref().is_none_or(Affiliation::is_empty)
    }
}

/// An author affiliation, parsed from a TEI `<affiliation>` element.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Affiliation {
    /// Institution name (`<orgName type="institution">`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub institution: Option<String>,
    /// Department name (`<orgName type="department">`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub department: Option<String>,
    /// Laboratory name (`<orgName type="laboratory">`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub laboratory: Option<String>,
    /// Postal address, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<Address>,
    /// Coordinates in the original PDF, when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coords: Option<Coords>,
}

impl Affiliation {
    /// Returns true if nothing is known about this affiliation.
    pub fn is_empty(&self) -> bool {
        self.institution.is_none()
            && self.department.is_none()
            && self.laboratory.is_none()
            && self.address.is_none()
    }
}

/// A postal address, parsed from a TEI `<address>` element.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Address {
    /// First address line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addr_line: Option<String>,
    /// Postal code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_code: Option<String>,
    /// City.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settlement: Option<String>,
    /// Country.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
}

/// A complete bibliographic record.
///
/// This is used both for the document header metadata ([`Header`]) and for
/// every parsed reference ([`Citation`]). Field semantics follow the GROBID
/// TEI encoding; see also the Go client (`github.com/schmettow/grobidclient`)
/// which this parser is modelled after.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Biblio {
    /// The authors of the work.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<Author>,
    /// The editors of the work.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub editors: Vec<Author>,
    /// The `xml:id` of the `<biblStruct>` element (e.g. `b12`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The original, unstructured reference string
    /// (`<note type="raw_reference">`), only present when
    /// `includeRawCitations` was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_reference: Option<String>,
    /// Publication date (`date/@when`), usually ISO-8601.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// Title of the work (article title).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Title of the containing book, when different from the article title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub book_title: Option<String>,
    /// Title of the containing series.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series_title: Option<String>,
    /// Journal title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub journal: Option<String>,
    /// Journal abbreviation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub journal_abbrev: Option<String>,
    /// Publisher.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    /// Issuing institution (e.g. for reports and theses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub institution: Option<String>,
    /// ISSN.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issn: Option<String>,
    /// Electronic ISSN.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eissn: Option<String>,
    /// Volume.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume: Option<String>,
    /// Issue.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    /// Page range, e.g. `235-243`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<String>,
    /// First page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_page: Option<String>,
    /// Last page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_page: Option<String>,
    /// A free-form note attached to the reference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// DOI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doi: Option<String>,
    /// PubMed ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pmid: Option<String>,
    /// PubMed Central ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pmcid: Option<String>,
    /// arXiv identifier, without the `arXiv:` prefix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arxiv_id: Option<String>,
    /// PII identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pii: Option<String>,
    /// ARK identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ark: Option<String>,
    /// ISTEX identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub istex_id: Option<String>,
    /// URL, usually from a `<ptr target="...">` element.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Coordinates in the original PDF, when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coords: Option<Coords>,
}

impl Biblio {
    /// Returns true if the record contains no meaningful information.
    ///
    /// Mirrors `IsEmpty` of the Go GROBID client: a record is considered
    /// empty when it has neither authors nor editors and none of the main
    /// bibliographic fields are set. Note that a `raw_reference` alone does
    /// not make a record non-empty.
    pub fn is_empty(&self) -> bool {
        if !self.authors.is_empty() || !self.editors.is_empty() {
            return false;
        }
        !any_nonempty(&[
            self.date.as_deref(),
            self.title.as_deref(),
            self.journal.as_deref(),
            self.publisher.as_deref(),
            self.volume.as_deref(),
            self.issue.as_deref(),
            self.pages.as_deref(),
            self.doi.as_deref(),
            self.pmid.as_deref(),
            self.pmcid.as_deref(),
            self.arxiv_id.as_deref(),
            self.url.as_deref(),
        ])
    }
}

/// Returns true if any of the given strings is non-empty.
fn any_nonempty(values: &[Option<&str>]) -> bool {
    values.iter().any(|v| v.is_some_and(|s| !s.is_empty()))
}

/// The bibliographic metadata of a processed document (its TEI header).
///
/// `Header` is a thin wrapper around [`Biblio`]; it dereferences to the
/// underlying record, so all bibliographic fields can be accessed directly,
/// e.g. `document.header.title`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Header {
    /// The header's bibliographic metadata.
    #[serde(flatten)]
    pub biblio: Biblio,
}

impl std::ops::Deref for Header {
    type Target = Biblio;
    fn deref(&self) -> &Biblio {
        &self.biblio
    }
}

impl std::ops::DerefMut for Header {
    fn deref_mut(&mut self) -> &mut Biblio {
        &mut self.biblio
    }
}

/// A single bibliographic reference (one `<biblStruct>` element), e.g. from
/// the reference list of a processed document.
///
/// `Citation` dereferences to its [`Biblio`] record, so all bibliographic
/// fields can be accessed directly, e.g. `citation.title`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Citation {
    /// Zero-based position of the reference within its list.
    pub index: usize,
    /// The bibliographic record.
    #[serde(flatten)]
    pub biblio: Biblio,
}

impl std::ops::Deref for Citation {
    type Target = Biblio;
    fn deref(&self) -> &Biblio {
        &self.biblio
    }
}

impl std::ops::DerefMut for Citation {
    fn deref_mut(&mut self) -> &mut Biblio {
        &mut self.biblio
    }
}

/// Kind of an in-text reference marker (`<ref type="...">`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefKind {
    /// Reference to a bibliographic entry (`type="bibr"`).
    #[serde(rename = "bibr")]
    Biblio,
    /// Reference to a figure (`type="figure"`).
    #[serde(rename = "figure")]
    Figure,
    /// Reference to a table (`type="table"`).
    #[serde(rename = "table")]
    Table,
    /// Reference to a formula (`type="formula"`).
    #[serde(rename = "formula")]
    Formula,
    /// Any other reference kind.
    #[serde(untagged)]
    Other(String),
}

impl Default for RefKind {
    fn default() -> Self {
        RefKind::Other(String::new())
    }
}

impl RefKind {
    /// Map a TEI `ref/@type` value onto a [`RefKind`].
    pub(crate) fn from_tei(value: Option<&str>) -> Self {
        match value {
            Some("bibr") => RefKind::Biblio,
            Some("figure") => RefKind::Figure,
            Some("table") => RefKind::Table,
            Some("formula") => RefKind::Formula,
            other => RefKind::Other(other.unwrap_or_default().to_string()),
        }
    }
}

/// An in-text reference marker, e.g. `(Smith et al., 2019)` or `(Fig. 1)`,
/// parsed from a TEI `<ref>` element.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MarkerRef {
    /// The kind of referenced object.
    pub kind: RefKind,
    /// The target of the reference, e.g. `#b0` or `#fig_1`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// The visible label of the marker, e.g. `(1)`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub label: String,
}

/// A sentence within a paragraph. Only present when GROBID was asked to
/// segment sentences (`segmentSentences=1`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Sentence {
    /// The sentence text.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// Coordinates in the original PDF, when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coords: Option<Coords>,
}

/// A paragraph (`<p>` element) of a document section.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Paragraph {
    /// The `xml:id`, only present when `generateIDs=1` was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The paragraph text, including the labels of inline reference
    /// markers.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// Sentence segmentation, only present when `segmentSentences=1` was
    /// requested.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sentences: Vec<Sentence>,
    /// In-text reference markers (bibliographic, figure, table, formula).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<MarkerRef>,
    /// Coordinates in the original PDF, when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coords: Option<Coords>,
}

/// A figure or table (`<figure>` element) of a document section.
///
/// GROBID encodes tables as `<figure type="table">` elements.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Figure {
    /// The `xml:id`, e.g. `fig_1`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The figure type, `table` for tables.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub figure_type: Option<String>,
    /// The figure head, e.g. `Fig. 1 .`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// The figure label, e.g. `1`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The figure caption (`<figDesc>`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    /// Coordinates in the original PDF, when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coords: Option<Coords>,
}

/// A mathematical formula (`<formula>` element) of a document section.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Formula {
    /// The `xml:id`, only present when `generateIDs=1` was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The textual content of the formula.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// Coordinates in the original PDF, when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coords: Option<Coords>,
}

/// A footnote (`<note>` element) of a document section.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Note {
    /// The `place` attribute, e.g. `foot`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    /// The note text.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// Coordinates in the original PDF, when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coords: Option<Coords>,
}

/// A list (`<list>` element) of a document section.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct List {
    /// The `type` attribute, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_type: Option<String>,
    /// The list items.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<String>,
}

/// An ordered content block of a [`Div`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Block {
    /// A paragraph.
    Paragraph(Paragraph),
    /// A figure or table.
    Figure(Figure),
    /// A mathematical formula.
    Formula(Formula),
    /// A footnote.
    Note(Note),
    /// A list.
    List(List),
    /// A nested subsection.
    Div(Box<Div>),
}

/// A document section (`<div>` element), holding its content in document
/// order. Sections can be nested arbitrarily deep, which reflects the full
/// heading hierarchy of the source document.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Div {
    /// The `type` attribute, e.g. `acknowledgement` or `references` for
    /// back matter sections.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub div_type: Option<String>,
    /// The section heading (`<head>`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// The section number (`head/@n`), e.g. `2.1`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<String>,
    /// The section content, in document order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<Block>,
    /// The `xml:id`, only present when `generateIDs=1` was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Coordinates in the original PDF, when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coords: Option<Coords>,
}

impl Div {
    /// All paragraphs of this section, including paragraphs of nested
    /// subsections, in document order.
    pub fn paragraphs(&self) -> Vec<&Paragraph> {
        fn collect<'a>(div: &'a Div, out: &mut Vec<&'a Paragraph>) {
            for block in &div.blocks {
                match block {
                    Block::Paragraph(p) => out.push(p),
                    Block::Div(sub) => collect(sub, out),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        collect(self, &mut out);
        out
    }

    /// All figures and tables of this section, including nested
    /// subsections, in document order.
    pub fn figures(&self) -> Vec<&Figure> {
        fn collect<'a>(div: &'a Div, out: &mut Vec<&'a Figure>) {
            for block in &div.blocks {
                match block {
                    Block::Figure(f) => out.push(f),
                    Block::Div(sub) => collect(sub, out),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        collect(self, &mut out);
        out
    }

    /// All nested subsections of this section (one level deep).
    pub fn subsections(&self) -> impl Iterator<Item = &Div> {
        self.blocks.iter().filter_map(|block| match block {
            Block::Div(div) => Some(div.as_ref()),
            _ => None,
        })
    }
}

/// A parsed GROBID TEI document, e.g. the response of
/// `processFulltextDocument` or `processHeaderDocument`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Document {
    /// The GROBID version that produced the document
    /// (`application/@version`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grobid_version: Option<String>,
    /// The processing timestamp (`application/@when`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grobid_timestamp: Option<String>,
    /// The MD5 checksum of the source PDF, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pdf_md5: Option<String>,
    /// The language of the document (`text/@xml:lang`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// The document's bibliographic metadata.
    pub header: Header,
    /// The abstract, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abstract_text: Option<String>,
    /// The document body as a hierarchy of sections.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub body: Vec<Div>,
    /// Footnotes that GROBID emitted directly under `<body>` (outside any
    /// section).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub body_notes: Vec<Note>,
    /// The acknowledgement section, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acknowledgements: Option<String>,
    /// The annex section, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annexes: Option<String>,
    /// The parsed bibliographic references.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<Citation>,
    /// Page dimensions of the source PDF, present when coordinates were
    /// requested.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub facsimile: Vec<Surface>,
}

impl Document {
    /// The full body text as a single string: the text of all body
    /// elements (headings, paragraphs, markers, captions, footnotes) joined
    /// with single spaces.
    pub fn body_text(&self) -> String {
        let mut parts = Vec::new();
        for div in &self.body {
            collect_div_text(div, &mut parts);
        }
        for note in &self.body_notes {
            parts.push(note.text.clone());
        }
        parts.join(" ")
    }

    /// Look up a citation by its `xml:id` (e.g. `b12`, or `#b12` with the
    /// leading `#` of an in-text reference target).
    pub fn find_citation(&self, id: &str) -> Option<&Citation> {
        let id = id.strip_prefix('#').unwrap_or(id);
        self.citations.iter().find(|c| c.id.as_deref() == Some(id))
    }
}

/// Collect all text fragments of a section, in document order.
fn collect_div_text(div: &Div, out: &mut Vec<String>) {
    if let Some(head) = &div.head {
        out.push(head.clone());
    }
    for block in &div.blocks {
        match block {
            Block::Paragraph(p) => out.push(p.text.clone()),
            Block::Figure(f) => {
                if let Some(head) = &f.head {
                    out.push(head.clone());
                }
                if let Some(caption) = &f.caption {
                    out.push(caption.clone());
                }
            }
            Block::Formula(f) => out.push(f.text.clone()),
            Block::Note(n) => out.push(n.text.clone()),
            Block::List(list) => out.extend(list.items.iter().cloned()),
            Block::Div(div) => collect_div_text(div, out),
        }
    }
}
