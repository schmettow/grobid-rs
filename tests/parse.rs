//! Tests for the TEI parser, using GROBID test fixtures. The assertions
//! mirror the test suite of the Go client
//! (<https://github.com/schmettow/grobidclient>), which this parser is
//! modelled after.

use grobid::tei::{parse_citation, parse_citation_list, parse_document};
use grobid::{Block, Coords, Error, RefKind, Surface};

const EXAMPLE_TEI: &str = include_str!("../testdata/document/example.tei.xml");
const SMALL_TEI: &str = include_str!("../testdata/small.xml");
const CITATION_LIST_TEI: &str = include_str!("../testdata/citation_list/example.tei.xml");
const EMPTY_TEI: &str = include_str!("../testdata/citation/empty.tei.xml");
const EMPTY_UNSTRUCTURED_TEI: &str =
    include_str!("../testdata/citation/empty_unstructured.tei.xml");

#[test]
fn test_example_document() {
    let doc = parse_document(EXAMPLE_TEI).expect("parse example document");
    let want = "Changes of patients' satisfaction with the health care services \
                in Lithuanian Health Promoting Hospitals network";
    assert_eq!(doc.header.title.as_deref(), Some(want));
    assert_eq!(doc.grobid_version.as_deref(), Some("0.5.5-fatcat"));
    assert_eq!(
        doc.grobid_timestamp.as_deref(),
        Some("2020-03-15T07:31+0000")
    );
    assert_eq!(doc.language.as_deref(), Some("en"));
    assert_eq!(doc.citations.len(), 15);

    // Header metadata: two named authors; the two affiliation-only
    // `<author>` elements are skipped.
    assert_eq!(doc.header.authors.len(), 2);
    assert_eq!(
        doc.header.authors[0].full_name.as_deref(),
        Some("Irena Misevičienė")
    );
    assert_eq!(
        doc.header.authors[1].full_name.as_deref(),
        Some("Žemyna Milašauskienė")
    );
    assert_eq!(doc.header.date.as_deref(), Some("2003"));
    assert_eq!(doc.header.journal.as_deref(), Some("MEDICINA"));
    assert_eq!(doc.header.volume.as_deref(), Some("39"));

    // A specific citation (b12, like the Go test).
    let ref_b12 = doc.find_citation("b12").expect("citation b12");
    assert_eq!(ref_b12.authors.len(), 3);
    let author0 = &ref_b12.authors[0];
    assert_eq!(author0.full_name.as_deref(), Some("K Tasa"));
    assert_eq!(author0.given_name.as_deref(), Some("K"));
    assert_eq!(author0.surname.as_deref(), Some("Tasa"));
    assert_eq!(
        ref_b12.journal.as_deref(),
        Some("Quality Management in Health Care")
    );
    assert_eq!(
        ref_b12.title.as_deref(),
        Some("Using patient feedback for quality improvement")
    );
    assert_eq!(ref_b12.date.as_deref(), Some("1996"));
    assert_eq!(ref_b12.pages.as_deref(), Some("206-225"));
    assert_eq!(ref_b12.volume.as_deref(), Some("8"));
    let raw_reference = "Tasa K, Baker R, Murray M. Using patient feedback for qua- lity \
                improvement. Quality Management in Health Care 1996;8:206-19.";
    assert_eq!(ref_b12.raw_reference.as_deref(), Some(raw_reference));
    // find_citation also accepts the "#b12" target form.
    assert!(doc.find_citation("#b12").is_some());
    assert!(doc.find_citation("nope").is_none());

    // Body hierarchy: four top-level sections; the first section head
    // repeats the article title.
    assert_eq!(doc.body.len(), 4);
    assert_eq!(doc.body[0].head.as_deref(), Some(want));
    assert_eq!(doc.body[1].head.as_deref(), Some("Material and methods"));
    assert_eq!(doc.body[2].head.as_deref(), Some("Results and discussion"));
    assert_eq!(doc.body[3].head.as_deref(), Some("Conclusions"));
    let first_paragraph = &doc.body[0].paragraphs()[0];
    assert!(first_paragraph.text.contains("The increasing competition"));
    assert!(first_paragraph
        .markers
        .iter()
        .any(|m| m.kind == RefKind::Biblio && m.target.as_deref() == Some("#b0")));

    // Back matter.
    let abstract_text = doc.abstract_text.as_ref().expect("abstract");
    assert!(abstract_text.starts_with("Key words: health care"));
    let acknowledgements = doc.acknowledgements.as_ref().expect("acknowledgements");
    assert!(acknowledgements.contains("We thank the chiefs"));
    assert!(doc.body_text().contains("MEDICINA (2003) 39 tomas"));
    // The footnote is emitted directly under <body>, outside any div.
    assert_eq!(doc.body_notes.len(), 1);
    assert_eq!(doc.body_notes[0].place.as_deref(), Some("foot"));
    assert!(doc.body_notes[0]
        .text
        .starts_with("MEDICINA (2003) 39 tomas"));
    assert!(doc.facsimile.is_empty());
}

#[test]
fn test_small_document() {
    let doc = parse_document(SMALL_TEI).expect("parse small document");
    assert_eq!(doc.grobid_version.as_deref(), Some("0.5.1-SNAPSHOT"));
    assert_eq!(
        doc.grobid_timestamp.as_deref(),
        Some("2018-04-02T00:31+0000")
    );
    assert_eq!(doc.language.as_deref(), Some("en"));
    assert_eq!(doc.header.title.as_deref(), Some("Dummy Example File"));
    assert_eq!(
        doc.header.book_title.as_deref(),
        Some("Dummy Example File. Journal of Fake News. pp. 1-2. ISSN 1234-5678")
    );
    assert_eq!(doc.header.date.as_deref(), Some("2000"));
    assert_eq!(
        doc.abstract_text.as_deref(),
        Some("Everything you ever wanted to know about nothing")
    );

    // Header authors: the author element without a persName is skipped.
    assert_eq!(doc.header.authors.len(), 2);
    let author = &doc.header.authors[0];
    assert_eq!(author.full_name.as_deref(), Some("Brewster Kahle"));
    assert_eq!(author.given_name.as_deref(), Some("Brewster"));
    assert_eq!(author.surname.as_deref(), Some("Kahle"));
    let affiliation = author.affiliation.as_ref().expect("affiliation");
    assert_eq!(
        affiliation.institution.as_deref(),
        Some("Technion-Israel Institute of Technology")
    );
    assert_eq!(
        affiliation.department.as_deref(),
        Some("Faculty ofAgricultrial Engineering")
    );
    assert_eq!(
        affiliation.laboratory.as_deref(),
        Some("Plant Physiology Laboratory")
    );
    let address = affiliation.address.as_ref().expect("address");
    assert_eq!(address.post_code.as_deref(), Some("32000"));
    assert_eq!(address.settlement.as_deref(), Some("Haifa"));
    assert_eq!(address.country.as_deref(), Some("Israel"));

    // Citations.
    assert_eq!(doc.citations.len(), 2);
    let citation = &doc.citations[0];
    assert_eq!(citation.title.as_deref(), Some("Everything is Wonderful"));
    assert_eq!(citation.journal.as_deref(), Some("Letters in the Alphabet"));
    assert_eq!(citation.volume.as_deref(), Some("20"));
    assert_eq!(citation.pages.as_deref(), Some("1-11"));
    assert_eq!(citation.first_page.as_deref(), Some("1"));
    assert_eq!(citation.last_page.as_deref(), Some("11"));
    assert_eq!(citation.date.as_deref(), Some("2001"));
    assert_eq!(citation.authors.len(), 1);
    assert_eq!(citation.authors[0].middle_name.as_deref(), Some("A"));
    assert_eq!(citation.authors[0].surname.as_deref(), Some("Seaperson"));
    assert_eq!(doc.citations[1].note.as_deref(), Some("author signed copy"));

    // Body: this older GROBID output emits all sections as flat, sibling
    // `<div>` elements (subsection nesting is covered elsewhere).
    assert_eq!(doc.body.len(), 4);
    assert_eq!(doc.body[0].number.as_deref(), Some("1"));
    assert_eq!(doc.body[1].number.as_deref(), Some("2"));
    assert_eq!(doc.body[2].number.as_deref(), Some("2.1"));
    assert_eq!(doc.body[3].number.as_deref(), Some("2.2"));
    assert!(doc.body[1].blocks.is_empty());
    assert_eq!(doc.body[2].head.as_deref(), Some("Meat"));
    assert_eq!(doc.body[3].head.as_deref(), Some("Potatos"));

    // Body text matches the Go client's joined body text.
    let want = "Introduction Everything starts somewhere, as somebody [1] once said. \
                In Depth Meat You know, for kids. Potatos QED.";
    assert_eq!(doc.body_text(), want);
}

#[test]
fn test_citation_list() {
    let citations = parse_citation_list(CITATION_LIST_TEI).expect("parse citation list");
    assert_eq!(citations.len(), 13);

    let c = &citations[3];
    assert_eq!(
        c.note.as_deref(),
        Some("The Research Handbook on International Environmental Law")
    );
    assert_eq!(c.authors.len(), 2);
    assert_eq!(c.authors[0].surname.as_deref(), Some("Uhlířová"));
    assert_eq!(c.authors[1].surname.as_deref(), Some("Drumbl"));
    assert_eq!(c.editors.len(), 3);
    assert_eq!(c.editors[0].surname.as_deref(), Some("Fitzmaurice"));
    assert_eq!(c.editors[1].surname.as_deref(), Some("Brus"));
    assert_eq!(c.editors[2].surname.as_deref(), Some("Merkouris"));

    assert_eq!(citations[4].authors[0].surname.as_deref(), Some("Sleytr"));
    assert_eq!(citations[4].authors[0].middle_name.as_deref(), Some("B"));

    assert_eq!(
        citations[7].title.as_deref(),
        Some("Global Hunger Index: The Challenge of Hidden Hunger")
    );

    assert_eq!(
        citations[10].doi.as_deref(),
        Some("10.1093/eurheartj/ehi890")
    );
    // The DOI URL is dropped when a DOI is present.
    assert_eq!(citations[10].url, None);

    assert_eq!(
        citations[11].title.as_deref(),
        Some("Devices, Measurements and Properties")
    );
    assert_eq!(
        citations[11].series_title.as_deref(),
        Some("Handbook of Optics")
    );
    assert_eq!(citations[11].publisher.as_deref(), Some("McGRAW-HILL"));

    let want = "Implications of abandoned shoreline features above Glacial Lake Duluth \
                levels along the north shore of the Superior Basin in the vicinity of the \
                Brule River";
    assert_eq!(citations[12].title.as_deref(), Some(want));
    let want = "Paper presented at the 13th Biennial Meeting of the American Quaternary \
                Association";
    assert_eq!(citations[12].book_title.as_deref(), Some(want));
    assert_eq!(
        citations[12].institution.as_deref(),
        Some("University of Minnesota")
    );
}

#[test]
fn test_empty_citations() {
    // Fully empty citations yield None for parse_citation, but are still
    // returned as (empty) entries by parse_citation_list.
    assert!(parse_citation(EMPTY_TEI).expect("parse").is_none());
    assert!(parse_citation(EMPTY_UNSTRUCTURED_TEI)
        .expect("parse")
        .is_none());

    let list = parse_citation_list(EMPTY_TEI).expect("parse list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].index, 0);
    assert_eq!(list[0].raw_reference, None);
    assert!(list[0].is_empty());

    let list = parse_citation_list(EMPTY_UNSTRUCTURED_TEI).expect("parse list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].raw_reference.as_deref(), Some("blah"));
}

#[test]
fn test_single_citation() {
    let xml = r#"
<biblStruct>
    <analytic>
        <title level="a" type="main">Mesh migration following abdominal hernia repair: a comprehensive review</title>
        <author>
            <persName
                xmlns="http://www.tei-c.org/ns/1.0">
                <forename type="first">H</forename>
                <forename type="middle">B</forename>
                <surname>Cunningham</surname>
            </persName>
        </author>
        <author>
            <persName
                xmlns="http://www.tei-c.org/ns/1.0">
                <forename type="first">J</forename>
                <forename type="middle">J</forename>
                <surname>Weis</surname>
            </persName>
        </author>
        <author>
            <persName
                xmlns="http://www.tei-c.org/ns/1.0">
                <forename type="first">L</forename>
                <forename type="middle">R</forename>
                <surname>Taveras</surname>
            </persName>
        </author>
        <author>
            <persName
                xmlns="http://www.tei-c.org/ns/1.0">
                <forename type="first">S</forename>
                <surname>Huerta</surname>
            </persName>
        </author>
        <idno type="DOI">10.1007/s10029-019-01898-9</idno>
        <idno type="PMID">30701369</idno>
    </analytic>
    <monogr>
        <title level="j">Hernia</title>
        <imprint>
            <biblScope unit="volume">23</biblScope>
            <biblScope unit="issue">2</biblScope>
            <biblScope unit="page" from="235" to="243" />
            <date type="published" when="2019-01-30" />
        </imprint>
    </monogr>
</biblStruct>"#;
    let citation = parse_citation(xml).expect("parse").expect("citation");
    assert!(!citation.is_empty());
    let want = "Mesh migration following abdominal hernia repair: a comprehensive review";
    assert_eq!(citation.title.as_deref(), Some(want));
    assert_eq!(citation.authors.len(), 4);
    assert_eq!(citation.authors[2].given_name.as_deref(), Some("L"));
    assert_eq!(citation.authors[2].middle_name.as_deref(), Some("R"));
    assert_eq!(citation.authors[2].surname.as_deref(), Some("Taveras"));
    assert_eq!(
        citation.authors[2].full_name.as_deref(),
        Some("L R Taveras")
    );
    assert_eq!(citation.doi.as_deref(), Some("10.1007/s10029-019-01898-9"));
    assert_eq!(citation.pmid.as_deref(), Some("30701369"));
    assert_eq!(citation.date.as_deref(), Some("2019-01-30"));
    assert_eq!(citation.pages.as_deref(), Some("235-243"));
    assert_eq!(citation.first_page.as_deref(), Some("235"));
    assert_eq!(citation.last_page.as_deref(), Some("243"));
    assert_eq!(citation.issue.as_deref(), Some("2"));
    assert_eq!(citation.journal.as_deref(), Some("Hernia"));
}

#[test]
fn test_small_snapshot() {
    // Serializes the fully parsed small.xml fixture to JSON. The snapshot
    // exercises the serde layer, including the flattened Header/Citation
    // records. Regenerate with TEST_SNAPSHOT=1.
    let doc = parse_document(SMALL_TEI).expect("parse small document");
    let got = serde_json::to_string_pretty(&doc).expect("serialize");
    let snapshot_path = "testdata/small.snapshot.json";
    match std::env::var("TEST_SNAPSHOT").as_deref() {
        Ok("1") | Ok("true") | Ok("yes") | Ok("on") => {
            std::fs::write(snapshot_path, format!("{got}\n")).expect("write snapshot");
        }
        _ => {
            let want = std::fs::read_to_string(snapshot_path).expect("read snapshot");
            assert_eq!(got.trim_end(), want.trim_end());
            // Round-trip: the snapshot must deserialize back into an equal
            // document (exercises the Deserialize impls, including the
            // flattened records).
            let round_tripped: grobid::Document = serde_json::from_str(&got).expect("deserialize");
            assert_eq!(doc, round_tripped);
        }
    }
}

#[test]
fn test_invalid_document() {
    assert!(matches!(
        parse_document("this is not XML"),
        Err(Error::Xml(_))
    ));
    assert!(matches!(
        parse_document("<xml></xml>"),
        Err(Error::InvalidDocument(_))
    ));
    // A TEI root without a teiHeader is rejected as well.
    assert!(matches!(
        parse_document("<TEI><text><body/></text></TEI>"),
        Err(Error::InvalidDocument(_))
    ));
}

#[test]
fn test_coords_parsing() {
    let coords = Coords::parse("1,53.80,194.57,58.71,9.29").expect("single box");
    assert_eq!(coords.boxes.len(), 1);
    assert_eq!(coords.boxes[0].page, 1);
    assert_eq!(coords.boxes[0].x, 53.80);
    assert_eq!(coords.boxes[0].y, 194.57);
    assert_eq!(coords.boxes[0].width, 58.71);
    assert_eq!(coords.boxes[0].height, 9.29);

    let coords = Coords::parse("10,317.03,183.61,223.16,7.55;10,317.03,192.57,223.21,7.55")
        .expect("two boxes");
    assert_eq!(coords.boxes.len(), 2);
    assert_eq!(coords.boxes[1].y, 192.57);

    assert!(matches!(
        Coords::parse("1,2,3"),
        Err(Error::InvalidCoords { .. })
    ));
    assert!(matches!(
        Coords::parse("1,2,3,4,5,6"),
        Err(Error::InvalidCoords { .. })
    ));
    assert!(matches!(
        Coords::parse("1,x,3,4,5"),
        Err(Error::InvalidCoords { .. })
    ));
}

/// A synthetic document exercising coordinates, sentence segmentation, the
/// document hierarchy and the references section.
const COORDINATED_TEI: &str = r##"
<TEI xmlns="http://www.tei-c.org/ns/1.0">
  <teiHeader>
    <encodingDesc>
      <appInfo>
        <application version="0.8.1" ident="GROBID" when="2024-01-01T00:00+0000"/>
      </appInfo>
    </encodingDesc>
    <fileDesc>
      <titleStmt><title level="a" type="main">Coordinated</title></titleStmt>
      <publicationStmt><publisher/></publicationStmt>
      <sourceDesc><biblStruct/></sourceDesc>
    </fileDesc>
  </teiHeader>
  <facsimile>
    <surface n="1" ulx="0.0" uly="0.0" lrx="612.0" lry="794.0"/>
    <surface n="2" ulx="0.0" uly="0.0" lrx="600.0" lry="800.0"/>
  </facsimile>
  <text xml:lang="en">
    <body>
      <div coords="2,10.0,20.0,30.0,40.0">
        <head n="1">Intro</head>
        <p coords="2,50.0,60.0,70.0,80.0">
          <s coords="2,51.0,61.0,71.0,81.0">Hello <ref type="bibr" target="#b0">(1)</ref>.</s>
          <s>Second sentence.</s>
        </p>
        <figure xml:id="fig_1" coords="3,1.0,2.0,3.0,4.0">
          <head>Fig. 1</head><label>1</label><figDesc>A caption</figDesc>
        </figure>
        <formula coords="3,5.0,6.0,7.0,8.0">x = 1</formula>
        <note place="foot">A footnote</note>
        <div>
          <head n="1.1">Sub</head>
          <p>Nested paragraph.</p>
        </div>
      </div>
    </body>
    <back>
      <div type="references">
        <listBibl>
          <biblStruct xml:id="b0" coords="4,317.03,183.61,223.16,7.55;4,317.03,192.57,223.21,7.55">
            <analytic><title level="a" type="main">A ref</title></analytic>
            <monogr><title level="j">A journal</title></monogr>
          </biblStruct>
        </listBibl>
      </div>
    </back>
  </text>
</TEI>
"##;

#[test]
fn test_coordinated_document() {
    let doc = parse_document(COORDINATED_TEI).expect("parse coordinated document");

    // Facsimile page dimensions.
    assert_eq!(
        doc.facsimile,
        vec![
            Surface {
                page: 1,
                width: 612.0,
                height: 794.0
            },
            Surface {
                page: 2,
                width: 600.0,
                height: 800.0
            },
        ]
    );

    // Section coordinates.
    let section = &doc.body[0];
    assert_eq!(section.head.as_deref(), Some("Intro"));
    assert_eq!(section.number.as_deref(), Some("1"));
    let coords = section.coords.as_ref().expect("section coords");
    assert_eq!(coords.boxes.len(), 1);
    assert_eq!(coords.boxes[0].page, 2);
    assert_eq!(coords.boxes[0].x, 10.0);

    // Paragraph with sentence segmentation and markers.
    let blocks = &section.blocks;
    let Block::Paragraph(paragraph) = &blocks[0] else {
        panic!("expected paragraph, got {:?}", blocks[0]);
    };
    assert_eq!(paragraph.text, "Hello (1) . Second sentence.");
    assert_eq!(paragraph.sentences.len(), 2);
    assert_eq!(paragraph.sentences[0].text, "Hello (1) .");
    assert_eq!(
        paragraph.sentences[0].coords.as_ref().unwrap().boxes[0].x,
        51.0
    );
    assert_eq!(paragraph.markers.len(), 1);
    assert_eq!(paragraph.markers[0].kind, RefKind::Biblio);
    assert_eq!(paragraph.markers[0].target.as_deref(), Some("#b0"));
    assert_eq!(paragraph.markers[0].label, "(1)");

    // Figure, formula and note blocks.
    let Block::Figure(figure) = &blocks[1] else {
        panic!("expected figure")
    };
    assert_eq!(figure.id.as_deref(), Some("fig_1"));
    assert_eq!(figure.head.as_deref(), Some("Fig. 1"));
    assert_eq!(figure.label.as_deref(), Some("1"));
    assert_eq!(figure.caption.as_deref(), Some("A caption"));
    assert!(figure.coords.is_some());
    let Block::Formula(formula) = &blocks[2] else {
        panic!("expected formula")
    };
    assert_eq!(formula.text, "x = 1");
    assert!(formula.coords.is_some());
    let Block::Note(note) = &blocks[3] else {
        panic!("expected note")
    };
    assert_eq!(note.place.as_deref(), Some("foot"));
    assert_eq!(note.text, "A footnote");

    // Nested subsection, in document order after the note.
    let Block::Div(subsection) = &blocks[4] else {
        panic!("expected nested div")
    };
    assert_eq!(subsection.head.as_deref(), Some("Sub"));
    assert_eq!(subsection.number.as_deref(), Some("1.1"));
    assert_eq!(subsection.paragraphs().len(), 1);
    assert_eq!(subsection.paragraphs()[0].text, "Nested paragraph.");
    assert_eq!(section.subsections().count(), 1);
    assert_eq!(section.paragraphs().len(), 2);

    // Citation with two coordinate boxes.
    assert_eq!(doc.citations.len(), 1);
    let citation = doc.find_citation("#b0").expect("citation by target");
    assert_eq!(citation.coords.as_ref().unwrap().boxes.len(), 2);
}
