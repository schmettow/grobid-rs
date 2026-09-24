//! Parsing of GROBID TEI-XML into strongly typed structures.
//!
//! The parsing strategy is modelled after the Go TEI parser
//! (<https://github.com/schmettow/grobidclient>, package `tei`), which is
//! itself closely modelled after the `grobid-tei-xml` Python package. Field
//! extraction, text joining and URL cleaning follow the behaviour of these
//! references.
//!
//! GROBID outputs XML in the TEI namespace (`http://www.tei-c.org/ns/1.0`),
//! but some services (e.g. `/api/processCitation`) return bare `<biblStruct>`
//! fragments without any namespace declaration. All element and attribute
//! lookups below therefore match on *local names only* and ignore
//! namespaces entirely, which makes the parser work with both flavours.

use roxmltree::Node;

use super::model::*;
use crate::error::InvalidDocument;
use crate::Error;

/// Parse a full GROBID TEI document, as returned by e.g.
/// `processFulltextDocument` or `processHeaderDocument`.
pub fn parse_document(xml: &str) -> Result<Document, Error> {
    let doc = roxmltree::Document::parse(xml)?;
    let root = doc.root_element();
    if !is_tag(&root, "TEI") {
        return Err(InvalidDocument::NotTei.into());
    }
    let Some(tei_header) = root.descendants().find(|n| is_tag(n, "teiHeader")) else {
        return Err(InvalidDocument::MissingTeiHeader.into());
    };
    let Some(app) = tei_header.descendants().find(|n| is_tag(n, "application")) else {
        return Err(InvalidDocument::MissingApplication.into());
    };

    let header = Header {
        biblio: parse_biblio(&tei_header)?,
    };
    let language = root
        .descendants()
        .find(|n| is_tag(n, "text"))
        .and_then(|t| attr(&t, "lang"))
        .map(str::to_string);
    let abstract_text = tei_header
        .descendants()
        .find(|n| is_tag(n, "abstract"))
        .and_then(|n| nonempty_text(&n));
    let facsimile = parse_facsimile(&root)?;
    let mut body_notes = Vec::new();
    let body = match root.descendants().find(|n| is_tag(n, "body")) {
        Some(body) => {
            for child in body.children() {
                if is_tag(&child, "note") {
                    body_notes.push(parse_note(&child)?);
                }
            }
            body.children()
                .filter(|c| is_tag(c, "div"))
                .map(|d| parse_div(&d))
                .collect::<Result<Vec<_>, _>>()?
        }
        None => Vec::new(),
    };
    let back = match root.descendants().find(|n| is_tag(n, "back")) {
        Some(back) => parse_back(&back)?,
        None => Back::default(),
    };
    // Citations live in the reference list; fall back to any biblStruct
    // outside the teiHeader when no reference section is present.
    let citations = if back.citations.is_empty() {
        root.descendants()
            .filter(|n| is_tag(n, "biblStruct") && !n.ancestors().any(|a| is_tag(&a, "teiHeader")))
            .enumerate()
            .map(|(i, bs)| parse_citation_el(&bs, i))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        back.citations
    };

    Ok(Document {
        grobid_version: nonempty_attr(&app, "version"),
        grobid_timestamp: nonempty_attr(&app, "when"),
        pdf_md5: find_desc_attr(&tei_header, "idno", "type", "MD5").and_then(|n| nonempty_text(&n)),
        language,
        header,
        abstract_text,
        body,
        body_notes,
        acknowledgements: back.acknowledgements,
        annexes: back.annexes,
        citations,
        facsimile,
    })
}

/// Parse a list of references, as returned by `/api/processCitationList` or
/// `/api/processReferences`. Handles both bare `<biblStruct>` responses and
/// full TEI documents.
pub fn parse_citation_list(xml: &str) -> Result<Vec<Citation>, Error> {
    let doc = roxmltree::Document::parse(xml)?;
    let root = doc.root_element();
    if is_tag(&root, "biblStruct") {
        return Ok(vec![parse_citation_el(&root, 0)?]);
    }
    root.descendants()
        .filter(|n| is_tag(n, "biblStruct"))
        .enumerate()
        .map(|(i, bs)| parse_citation_el(&bs, i))
        .collect()
}

/// Parse a single reference, as returned by `/api/processCitation`. Returns
/// `None` when the response contains no usable citation.
pub fn parse_citation(xml: &str) -> Result<Option<Citation>, Error> {
    let mut list = parse_citation_list(xml)?;
    let citation = list.drain(..).next();
    Ok(citation.filter(|c| !c.is_empty()))
}

/// Parse a `<biblStruct>` element into a citation with the given index.
fn parse_citation_el(node: &Node, index: usize) -> Result<Citation, Error> {
    Ok(Citation {
        index,
        biblio: parse_biblio(node)?,
    })
}

/// Parse a `<biblStruct>` (or, for document headers, the whole `<teiHeader>`)
/// into a bibliographic record.
fn parse_biblio(node: &Node) -> Result<Biblio, Error> {
    let mut biblio = Biblio::default();

    for author_el in node.descendants().filter(|n| is_tag(n, "author")) {
        if let Some(author) = parse_author(&author_el)? {
            biblio.authors.push(author);
        }
    }
    for editor_el in node.descendants().filter(|n| is_tag(n, "editor")) {
        biblio.editors.extend(parse_editor(&editor_el)?);
    }
    for contrib in node
        .descendants()
        .filter(|n| is_tag(n, "contributor") && attr(n, "role") == Some("editor"))
    {
        biblio.editors.extend(parse_editor(&contrib)?);
    }

    biblio.id = attr(node, "id").map(str::to_string);
    biblio.raw_reference =
        find_desc_attr(node, "note", "type", "raw_reference").and_then(|n| nonempty_text(&n));
    biblio.title = find_desc_attr(node, "title", "type", "main").and_then(|n| nonempty_text(&n));
    biblio.journal = find_desc_attr(node, "title", "level", "j").and_then(|n| nonempty_text(&n));
    biblio.journal_abbrev = node
        .descendants()
        .find(|n| {
            is_tag(n, "title") && attr(n, "level") == Some("j") && attr(n, "type") == Some("abbrev")
        })
        .and_then(|n| nonempty_text(&n));
    biblio.series_title =
        find_desc_attr(node, "title", "level", "s").and_then(|n| nonempty_text(&n));

    // Book title: a level="m" title without a type attribute. When there is
    // no article title, the book title becomes the title of the record.
    if let Some(book_title) = node
        .descendants()
        .find(|n| is_tag(n, "title") && attr(n, "level") == Some("m") && attr(n, "type").is_none())
    {
        biblio.book_title = nonempty_text(&book_title);
    }
    if biblio.book_title.is_some() && biblio.title.is_none() {
        biblio.title = biblio.book_title.take();
    }

    if let Some(note) = node
        .descendants()
        .find(|n| is_tag(n, "note") && attr(n, "type").is_none())
    {
        biblio.note = nonempty_text(&note);
    }

    biblio.publisher = node
        .descendants()
        .find(|n| is_tag(n, "publicationStmt"))
        .and_then(|ps| first_child(&ps, "publisher"))
        .and_then(|p| nonempty_text(&p));
    if biblio.publisher.is_none() {
        biblio.publisher = node
            .descendants()
            .find(|n| is_tag(n, "imprint"))
            .and_then(|im| first_child(&im, "publisher"))
            .and_then(|p| nonempty_text(&p));
    }

    biblio.institution = node
        .descendants()
        .find(|n| is_tag(n, "respStmt"))
        .and_then(|rs| first_child(&rs, "orgName"))
        .and_then(|o| nonempty_text(&o));

    biblio.volume =
        find_desc_attr(node, "biblScope", "unit", "volume").and_then(|n| nonempty_text(&n));
    biblio.issue =
        find_desc_attr(node, "biblScope", "unit", "issue").and_then(|n| nonempty_text(&n));
    if let Some(scope) = find_desc_attr(node, "biblScope", "unit", "page") {
        let from = attr(&scope, "from");
        let to = attr(&scope, "to");
        biblio.first_page = from.map(str::to_string);
        biblio.last_page = to.map(str::to_string);
        biblio.pages = match (from, to) {
            (Some(from), Some(to)) => Some(format!("{from}-{to}")),
            _ => nonempty_text(&scope),
        };
    }

    biblio.doi = find_desc_attr(node, "idno", "type", "DOI").and_then(|n| nonempty_text(&n));
    biblio.pmid = find_desc_attr(node, "idno", "type", "PMID").and_then(|n| nonempty_text(&n));
    biblio.pmcid = find_desc_attr(node, "idno", "type", "PMCID").and_then(|n| nonempty_text(&n));
    biblio.arxiv_id = find_desc_attr(node, "idno", "type", "arXiv")
        .and_then(|n| nonempty_text(&n))
        .map(|id| id.strip_prefix("arXiv:").unwrap_or(&id).to_string());
    biblio.pii = find_desc_attr(node, "idno", "type", "PII").and_then(|n| nonempty_text(&n));
    biblio.ark = find_desc_attr(node, "idno", "type", "ark").and_then(|n| nonempty_text(&n));
    biblio.istex_id =
        find_desc_attr(node, "idno", "type", "istexId").and_then(|n| nonempty_text(&n));
    biblio.issn = find_desc_attr(node, "idno", "type", "ISSN").and_then(|n| nonempty_text(&n));
    biblio.eissn = find_desc_attr(node, "idno", "type", "eISSN").and_then(|n| nonempty_text(&n));

    if let Some(date) = find_desc_attr(node, "date", "type", "published") {
        biblio.date = attr(&date, "when")
            .filter(|w| !w.is_empty())
            .map(str::to_string);
    }

    if let Some(ptr) = node
        .descendants()
        .find(|n| is_tag(n, "ptr") && attr(n, "target").is_some())
    {
        biblio.url = attr(&ptr, "target").map(clean_url);
    }
    if biblio.doi.is_some() {
        let is_doi_url = biblio
            .url
            .as_deref()
            .is_some_and(|u| u.contains("://doi.org/") || u.contains("://dx.doi.org/"));
        if is_doi_url {
            biblio.url = None;
        }
    }

    biblio.coords = coords_of(node)?;
    Ok(biblio)
}

/// Parse a `<persName>` element into an author.
fn parse_pers_name(node: &Node) -> Result<Author, Error> {
    Ok(Author {
        full_name: nonempty_text(node),
        given_name: first_child_attr(node, "forename", "type", "first")
            .and_then(|n| nonempty_text(&n)),
        middle_name: first_child_attr(node, "forename", "type", "middle")
            .and_then(|n| nonempty_text(&n)),
        surname: first_child(node, "surname").and_then(|n| nonempty_text(&n)),
        coords: coords_of(node)?,
        ..Author::default()
    })
}

/// Parse an `<author>` element, including header-only extras such as email,
/// ORCID and affiliation. Returns `None` when the element carries no name.
fn parse_author(node: &Node) -> Result<Option<Author>, Error> {
    let Some(pers_name) = first_child(node, "persName") else {
        return Ok(None);
    };
    let mut author = parse_pers_name(&pers_name)?;
    if let Some(idno) = find_desc_attr(node, "idno", "type", "ORCID") {
        author.orcid = nonempty_text(&idno);
    }
    if let Some(email) = first_child(node, "email") {
        author.email = nonempty_text(&email);
    }
    if let Some(affiliation) = first_child(node, "affiliation") {
        author.affiliation = parse_affiliation(&affiliation)?;
    }
    Ok(Some(author))
}

/// Parse an `<editor>` (or `<contributor role="editor">`) element. May
/// contain several `<persName>` elements, or a bare name string.
fn parse_editor(node: &Node) -> Result<Vec<Author>, Error> {
    let pers_names: Vec<_> = node.children().filter(|c| is_tag(c, "persName")).collect();
    if pers_names.is_empty() {
        if node.children().next().is_none() {
            let raw = node.text().unwrap_or("");
            let trimmed = raw.trim();
            if trimmed.len() > 2 {
                return Ok(vec![Author {
                    full_name: Some(trimmed.to_string()),
                    ..Author::default()
                }]);
            }
        }
        return Ok(Vec::new());
    }
    pers_names.iter().map(parse_pers_name).collect()
}

/// Parse an `<affiliation>` element. Returns `None` when the element carries
/// no information.
fn parse_affiliation(node: &Node) -> Result<Option<Affiliation>, Error> {
    let mut affiliation = Affiliation::default();
    for org in node.children().filter(|c| is_tag(c, "orgName")) {
        match attr(&org, "type") {
            Some("institution") => affiliation.institution = nonempty_text(&org),
            Some("department") => affiliation.department = nonempty_text(&org),
            Some("laboratory") => affiliation.laboratory = nonempty_text(&org),
            _ => {}
        }
    }
    if let Some(address) = first_child(node, "address") {
        affiliation.address = Some(Address {
            addr_line: first_child(&address, "addrLine").and_then(|n| nonempty_text(&n)),
            post_code: first_child(&address, "postCode").and_then(|n| nonempty_text(&n)),
            settlement: first_child(&address, "settlement").and_then(|n| nonempty_text(&n)),
            country: first_child(&address, "country").and_then(|n| nonempty_text(&n)),
        });
    }
    affiliation.coords = coords_of(node)?;
    if affiliation.is_empty() {
        Ok(None)
    } else {
        Ok(Some(affiliation))
    }
}

/// Parsed back matter of a document.
#[derive(Default)]
struct Back {
    acknowledgements: Option<String>,
    annexes: Option<String>,
    citations: Vec<Citation>,
}

/// Parse the `<back>` element: acknowledgement, annex and references.
fn parse_back(node: &Node) -> Result<Back, Error> {
    let mut back = Back::default();
    let mut acknowledgements: Vec<String> = Vec::new();
    let mut annexes: Vec<String> = Vec::new();
    for div in node.children().filter(|c| is_tag(c, "div")) {
        match attr(&div, "type") {
            Some("acknowledgement") => {
                if let Some(text) = nonempty_text(&div) {
                    acknowledgements.push(text);
                }
            }
            Some("annex") => {
                if let Some(text) = nonempty_text(&div) {
                    annexes.push(text);
                }
            }
            Some("references") => {
                for (i, bs) in div
                    .descendants()
                    .filter(|n| is_tag(n, "biblStruct"))
                    .enumerate()
                {
                    back.citations.push(parse_citation_el(&bs, i)?);
                }
            }
            _ => {}
        }
    }
    if !acknowledgements.is_empty() {
        back.acknowledgements = Some(acknowledgements.join(" "));
    }
    if !annexes.is_empty() {
        back.annexes = Some(annexes.join(" "));
    }
    Ok(back)
}

/// Parse the `<facsimile>` element with page dimensions.
fn parse_facsimile(root: &Node) -> Result<Vec<Surface>, Error> {
    let Some(facsimile) = root.children().find(|n| is_tag(n, "facsimile")) else {
        return Ok(Vec::new());
    };
    let mut surfaces = Vec::new();
    for surface in facsimile.children().filter(|n| is_tag(n, "surface")) {
        let page = attr(&surface, "n")
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0);
        let ulx = attr(&surface, "ulx")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        let uly = attr(&surface, "uly")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        let lrx = attr(&surface, "lrx")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        let lry = attr(&surface, "lry")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        surfaces.push(Surface {
            page,
            width: lrx - ulx,
            height: lry - uly,
        });
    }
    Ok(surfaces)
}

/// Parse a section (`<div>`) and its nested content.
fn parse_div(node: &Node) -> Result<Div, Error> {
    let head = first_child(node, "head");
    let mut div = Div {
        div_type: attr(node, "type").map(str::to_string),
        head: head.as_ref().and_then(|n| nonempty_text(n)),
        number: head.as_ref().and_then(|h| attr(h, "n")).map(str::to_string),
        id: attr(node, "id").map(str::to_string),
        coords: coords_of(node)?,
        ..Div::default()
    };
    for child in node.children() {
        if !child.is_element() {
            continue;
        }
        let block = match child.tag_name().name() {
            "p" => Block::Paragraph(parse_paragraph(&child)?),
            "figure" => Block::Figure(parse_figure(&child)?),
            "formula" => Block::Formula(parse_formula(&child)?),
            "note" => Block::Note(parse_note(&child)?),
            "list" => Block::List(parse_list(&child)?),
            "div" => Block::Div(Box::new(parse_div(&child)?)),
            _ => continue,
        };
        div.blocks.push(block);
    }
    Ok(div)
}

/// Parse a paragraph, including optional sentence segmentation and inline
/// reference markers.
fn parse_paragraph(node: &Node) -> Result<Paragraph, Error> {
    let sentences = node
        .children()
        .filter(|c| is_tag(c, "s"))
        .map(|s| {
            Ok(Sentence {
                text: text_of(&s),
                coords: coords_of(&s)?,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let markers = node
        .descendants()
        .filter(|n| is_tag(n, "ref"))
        .map(|r| MarkerRef {
            kind: RefKind::from_tei(attr(&r, "type")),
            target: attr(&r, "target").map(str::to_string),
            label: text_of(&r),
        })
        .collect();
    Ok(Paragraph {
        id: attr(node, "id").map(str::to_string),
        text: text_of(node),
        sentences,
        markers,
        coords: coords_of(node)?,
    })
}

/// Parse a `<figure>` (or `<figure type="table">`) element.
fn parse_figure(node: &Node) -> Result<Figure, Error> {
    Ok(Figure {
        id: attr(node, "id").map(str::to_string),
        figure_type: attr(node, "type").map(str::to_string),
        head: first_child(node, "head").and_then(|n| nonempty_text(&n)),
        label: first_child(node, "label").and_then(|n| nonempty_text(&n)),
        caption: first_child(node, "figDesc").and_then(|n| nonempty_text(&n)),
        coords: coords_of(node)?,
    })
}

/// Parse a `<formula>` element.
fn parse_formula(node: &Node) -> Result<Formula, Error> {
    Ok(Formula {
        id: attr(node, "id").map(str::to_string),
        text: text_of(node),
        coords: coords_of(node)?,
    })
}

/// Parse a `<note>` element.
fn parse_note(node: &Node) -> Result<Note, Error> {
    Ok(Note {
        place: attr(node, "place").map(str::to_string),
        text: text_of(node),
        coords: coords_of(node)?,
    })
}

/// Parse a `<list>` element.
fn parse_list(node: &Node) -> Result<List, Error> {
    Ok(List {
        list_type: attr(node, "type").map(str::to_string),
        items: node
            .children()
            .filter(|c| is_tag(c, "item"))
            .map(|item| text_of(&item))
            .collect(),
    })
}

// ---------------------------------------------------------------------------
// XML helpers. All lookups match local names only, ignoring the TEI
// namespace, so that both namespaced documents and bare fragments work.
// ---------------------------------------------------------------------------

/// Returns true if the node is an element with the given local tag name.
fn is_tag(node: &Node, tag: &str) -> bool {
    node.is_element() && node.tag_name().name() == tag
}

/// Returns the value of the attribute with the given local name, if present.
/// The attribute namespace (e.g. `xml:id`) is ignored.
fn attr<'a, 'input>(node: &Node<'a, 'input>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|a| a.name() == name)
        .map(|a| a.value())
}

/// Returns the value of the given attribute when it is non-empty,
/// with surrounding whitespace trimmed.
fn nonempty_attr(node: &Node, name: &str) -> Option<String> {
    attr(node, name)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// Returns the first element child with the given local tag name.
fn first_child<'a, 'input>(node: &Node<'a, 'input>, tag: &str) -> Option<Node<'a, 'input>> {
    node.children().find(|c| is_tag(c, tag))
}

/// Returns the first element child with the given local tag name and
/// attribute value.
fn first_child_attr<'a, 'input>(
    node: &Node<'a, 'input>,
    tag: &str,
    attr_name: &str,
    attr_value: &str,
) -> Option<Node<'a, 'input>> {
    node.children()
        .find(|c| is_tag(c, tag) && attr(c, attr_name) == Some(attr_value))
}

/// Returns the first descendant with the given local tag name and attribute
/// value.
fn find_desc_attr<'a, 'input>(
    node: &Node<'a, 'input>,
    tag: &str,
    attr_name: &str,
    attr_value: &str,
) -> Option<Node<'a, 'input>> {
    node.descendants()
        .find(|n| is_tag(n, tag) && attr(n, attr_name) == Some(attr_value))
}

/// Returns the `@coords` attribute of the node, parsed into bounding boxes.
fn coords_of(node: &Node) -> Result<Option<Coords>, Error> {
    match attr(node, "coords") {
        Some(value) => Coords::parse(value).map(Some),
        None => Ok(None),
    }
}

/// All text fragments of the subtree rooted at the node, in document order,
/// with each fragment trimmed; empty fragments are dropped. The result is
/// joined with single spaces, mirroring the text handling of the Go client.
fn text_of(node: &Node) -> String {
    let mut fragments: Vec<&str> = Vec::new();
    collect_text(node, &mut fragments);
    fragments.join(" ")
}

/// Returns the joined text of the node, or `None` when it is empty.
fn nonempty_text(node: &Node) -> Option<String> {
    let text = text_of(node);
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Recursively collect trimmed text fragments in document order.
///
/// Note: roxmltree's `children()` includes text nodes, so only element
/// children are recursed into; the direct text is captured by `text()` and
/// the text after each element by its `tail()`.
fn collect_text<'a, 'input>(node: &Node<'a, 'input>, out: &mut Vec<&'a str>) {
    if let Some(text) = node.text() {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            out.push(trimmed);
        }
    }
    for child in node.children().filter(|c| c.is_element()) {
        collect_text(&child, out);
        if let Some(tail) = child.tail() {
            let trimmed = tail.trim();
            if !trimmed.is_empty() {
                out.push(trimmed);
            }
        }
    }
}

/// Basic URL cleaning, based on issues observed in the wild.
fn clean_url(url: &str) -> String {
    let url = url.trim();
    if url.is_empty() {
        return url.to_string();
    }
    let url = url.strip_suffix(".Lastaccessed").unwrap_or(url);
    let url = url.strip_prefix('<').unwrap_or(url);
    let url = url.split('>').next().unwrap_or(url);
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_url() {
        let cases = [
            ("", ""),
            ("http://archive.org", "http://archive.org"),
            ("http://archive.org.Lastaccessed", "http://archive.org"),
            ("<http://archive.org.Lastaccessed", "http://archive.org"),
            ("<http://example.org>", "http://example.org"),
            ("  http://example.org  ", "http://example.org"),
        ];
        for (input, want) in cases {
            assert_eq!(clean_url(input), want, "input: {input:?}");
        }
    }
}
