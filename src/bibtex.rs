//! BibTeX rendering for parsed bibliographic records.
//!
//! This module adapts the strongly typed [`crate::tei::Biblio`] records onto
//! the [`biblatex`] crate, which handles BibTeX/BibLaTeX parsing, writing
//! and field typing. Using a full-fidelity BibTeX library (instead of
//! hand-rolled string formatting) gives proper escaping, typed person lists
//! and dates, and opens the door to reading back `.bib` files — e.g. for
//! deduplicating or correcting references.
//!
//! The `pdf2bibtex` and `refs2bibtex` examples use these helpers.

use std::collections::HashSet;

use biblatex::{Chunk, Date, DateValue, Datetime, Entry, EntryType, Person, Spanned};

use crate::tei::{Author, Biblio};

/// Pick a BibLaTeX entry type based on the available metadata.
///
/// * [`EntryType::Article`] for journal articles,
/// * [`EntryType::InCollection`] for chapters in a book,
/// * [`EntryType::InProceedings`] for conference series papers,
/// * [`EntryType::TechReport`] for records with an issuing institution,
/// * [`EntryType::Book`] for records with a publisher,
/// * [`EntryType::Misc`] otherwise.
pub fn entry_type(biblio: &Biblio) -> EntryType {
    if biblio.journal.is_some() {
        EntryType::Article
    } else if biblio.book_title.is_some() {
        EntryType::InCollection
    } else if biblio.series_title.is_some() {
        EntryType::InProceedings
    } else if biblio.institution.is_some() {
        EntryType::TechReport
    } else if biblio.publisher.is_some() {
        EntryType::Book
    } else {
        EntryType::Misc
    }
}

/// Extract the publication year (first four digits) from the date.
pub fn year(biblio: &Biblio) -> Option<String> {
    let date = biblio.date.as_deref()?;
    let digits: String = date
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .take(4)
        .collect();
    if digits.is_empty() {
        None
    } else {
        Some(digits)
    }
}

/// Suggest a BibTeX key from the first author's surname and the publication
/// year, e.g. `Kahle2000`. Falls back to `ref` when neither is known. The
/// key is filtered to ASCII alphanumerics.
///
/// Use [`unique_key`] when the suggested key must be collision-free.
pub fn suggest_key(biblio: &Biblio) -> String {
    let surname = biblio
        .authors
        .first()
        .and_then(|author| author.surname.as_deref())
        .unwrap_or_default();
    let base = format!("{surname}{}", year(biblio).unwrap_or_default());
    let key: String = base.chars().filter(char::is_ascii_alphanumeric).collect();
    if key.is_empty() {
        "ref".to_string()
    } else {
        key
    }
}

/// Return a citation key that is unique within `used`, derived from
/// [`suggest_key`] and extended with `-2`, `-3`, ... suffixes on
/// collisions. The returned key is inserted into `used`.
///
/// For deterministic output, feed the records in a stable order (e.g.
/// sorted by [`suggest_key`]).
pub fn unique_key(biblio: &Biblio, used: &mut HashSet<String>) -> String {
    let base = suggest_key(biblio);
    if used.insert(base.clone()) {
        return base;
    }
    let mut suffix = 2usize;
    loop {
        let key = format!("{base}-{suffix}");
        if used.insert(key.clone()) {
            return key;
        }
        suffix += 1;
    }
}

/// Convert a bibliographic record into a typed BibLaTeX entry.
///
/// Authors and editors become typed person lists, the GROBID date (year,
/// year-month or year-month-day) becomes a typed date, and the remaining
/// fields become plain text values. The returned entry can be serialized
/// with [`Entry::to_bibtex_string`], inspected with the typed getters, or
/// inserted into a [`biblatex::Bibliography`].
pub fn to_entry(key: &str, biblio: &Biblio) -> Entry {
    let mut entry = Entry::new(key.to_string(), entry_type(biblio));

    let authors = persons(&biblio.authors);
    if !authors.is_empty() {
        entry.set_as("author", &authors);
    }
    let editors = persons(&biblio.editors);
    if !editors.is_empty() {
        entry.set_as("editor", &editors);
    }
    for (name, value) in [
        ("title", &biblio.title),
        ("booktitle", &biblio.book_title),
        ("journaltitle", &biblio.journal),
        ("series", &biblio.series_title),
        ("volume", &biblio.volume),
        ("number", &biblio.issue),
        ("pages", &biblio.pages),
        ("publisher", &biblio.publisher),
        ("institution", &biblio.institution),
        ("doi", &biblio.doi),
        ("url", &biblio.url),
        ("issn", &biblio.issn),
        ("note", &biblio.note),
    ] {
        set_text(&mut entry, name, value);
    }
    if let Some(date) = date(&biblio.date) {
        entry.set_as("date", &date);
    }
    entry
}

/// Render one record as a BibTeX entry.
pub fn format_entry(key: &str, biblio: &Biblio) -> String {
    to_entry(key, biblio)
        .to_bibtex_string()
        .expect("entries built from typed values always serialize")
}

/// Map GROBID authors onto typed BibLaTeX persons. Middle names are folded
/// into the given name; authors with neither surname nor full name are
/// dropped.
fn persons(authors: &[Author]) -> Vec<Person> {
    authors.iter().filter_map(person).collect()
}

fn person(author: &Author) -> Option<Person> {
    let given = [author.given_name.as_deref(), author.middle_name.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    let name = author
        .surname
        .as_deref()
        .filter(|surname| !surname.is_empty())
        .map(str::to_string)
        .or_else(|| author.full_name.clone().filter(|n| !n.is_empty()))?;
    Some(Person {
        name,
        given_name: given,
        prefix: String::new(),
        suffix: String::new(),
        id: None,
        prefix_initials: None,
        given_initials: None,
        use_prefix: None,
    })
}

/// Parse a GROBID date (`YYYY`, `YYYY-MM` or `YYYY-MM-DD`) into a typed
/// BibLaTeX date. Dates without at least a four-digit year are dropped.
fn date(date: &Option<String>) -> Option<Date> {
    let raw = date.as_deref()?;
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).take(8).collect();
    let year = digits.get(0..4)?.parse::<i32>().ok()?;
    let month = digits
        .get(4..6)
        .and_then(|m| m.parse::<u8>().ok())
        .filter(|m| (1..=12).contains(m))
        .map(|m| m - 1);
    let day = match month {
        Some(_) => digits
            .get(6..8)
            .and_then(|d| d.parse::<u8>().ok())
            .filter(|d| (1..=31).contains(d))
            .map(|d| d - 1),
        None => None,
    };
    Some(Date {
        value: DateValue::At(Datetime {
            year,
            month,
            day,
            time: None,
        }),
        uncertain: false,
        approximate: false,
    })
}

/// Set a field from a plain-text value, when present and non-empty.
fn set_text(entry: &mut Entry, name: &str, value: &Option<String>) {
    if let Some(value) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        entry.set(
            name,
            vec![Spanned::detached(Chunk::Normal(value.to_string()))],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use biblatex::ChunksExt;

    fn author(surname: &str, given: Option<&str>) -> Author {
        Author {
            surname: Some(surname.to_string()),
            given_name: given.map(str::to_string),
            ..Author::default()
        }
    }

    #[test]
    fn test_suggest_key() {
        let biblio = Biblio {
            authors: vec![author("Milašauskienė", None)],
            date: Some("2003".to_string()),
            ..Biblio::default()
        };
        assert_eq!(suggest_key(&biblio), "Milaauskien2003");

        // Without author and date the key falls back to "ref"; uniqueness
        // is handled by unique_key.
        assert_eq!(suggest_key(&Biblio::default()), "ref");
    }

    #[test]
    fn test_year() {
        for (date, want) in [
            (Some("2019-01-30".to_string()), Some("2019")),
            (Some("2003".to_string()), Some("2003")),
            (Some("no digits".to_string()), None),
            (None, None),
        ] {
            let biblio = Biblio {
                date,
                ..Biblio::default()
            };
            assert_eq!(year(&biblio).as_deref(), want);
        }
    }

    #[test]
    fn test_entry_type() {
        assert_eq!(
            entry_type(&Biblio {
                journal: Some("X".to_string()),
                ..Biblio::default()
            }),
            EntryType::Article
        );
        assert_eq!(
            entry_type(&Biblio {
                book_title: Some("X".to_string()),
                ..Biblio::default()
            }),
            EntryType::InCollection
        );
        assert_eq!(
            entry_type(&Biblio {
                institution: Some("X".to_string()),
                ..Biblio::default()
            }),
            EntryType::TechReport
        );
        assert_eq!(
            entry_type(&Biblio {
                publisher: Some("X".to_string()),
                ..Biblio::default()
            }),
            EntryType::Book
        );
        assert_eq!(entry_type(&Biblio::default()), EntryType::Misc);
    }

    #[test]
    fn test_to_entry() {
        let mut biblio = Biblio::default();
        biblio.authors.push(author("Kahle", Some("Brewster")));
        biblio.authors.push(author("Doe", Some("J")));
        biblio.title = Some("Dummy Example File".to_string());
        biblio.journal = Some("Letters in the Alphabet".to_string());
        biblio.date = Some("2000-03-01".to_string());
        biblio.volume = Some("20".to_string());
        biblio.doi = Some("10.1234/example".to_string());

        let entry = to_entry("kahle2000", &biblio);
        assert_eq!(entry.entry_type, EntryType::Article);
        // The typed values round-trip through the field chunks.
        let authors: Vec<Person> = entry.get_as("author").expect("typed authors");
        assert_eq!(authors.len(), 2);
        assert_eq!(authors[0].name, "Kahle");
        assert_eq!(authors[0].given_name, "Brewster");
        let date: biblatex::Date = entry.get_as("date").expect("typed date");
        assert_eq!(
            date.value,
            DateValue::At(Datetime {
                year: 2000,
                month: Some(2),
                day: Some(0),
                time: None,
            })
        );
        assert_eq!(
            entry.get("doi").expect("doi").format_verbatim(),
            "10.1234/example"
        );
    }

    #[test]
    fn test_format_entry() {
        let mut biblio = Biblio::default();
        biblio.authors.push(author("Kahle", Some("Brewster")));
        biblio.authors.push(author("Doe", Some("J")));
        biblio.title = Some("Dummy Example File".to_string());
        biblio.date = Some("2000".to_string());
        biblio.volume = Some("20".to_string());

        let entry = format_entry("kahle2000", &biblio);
        assert!(entry.starts_with("@misc{kahle2000,"));
        assert!(entry.contains("author = {Kahle, Brewster and Doe, J},"));
        assert!(entry.contains("title = {Dummy Example File},"));
        assert!(entry.contains("year = {2000},"));
        assert!(entry.contains("volume = {20},"));
        assert!(entry.ends_with('}'));
    }

    #[test]
    fn test_roundtrip() {
        // Entries rendered to a .bib file must parse back into the same
        // typed values - the foundation for reading and deduplicating
        // references in the future.
        let mut biblio = Biblio::default();
        biblio.authors.push(author("Milašauskienė", Some("Žemyna")));
        biblio.title = Some("Dummy & \"quoted\" Example".to_string());
        biblio.journal = Some("A Journal".to_string());
        biblio.date = Some("2003-06-15".to_string());
        let key = unique_key(&biblio, &mut HashSet::new());
        let bibtex = format_entry(&key, &biblio);

        let bibliography = biblatex::Bibliography::parse(&bibtex).expect("parse output");
        let entry = bibliography.get(&key).expect("entry by key");
        assert_eq!(entry.entry_type, EntryType::Article);
        let authors: Vec<Person> = entry.get_as("author").expect("typed authors");
        assert_eq!(authors[0].name, "Milašauskienė");
        assert_eq!(authors[0].given_name, "Žemyna");
        // The BibTeX writer expands the date into year/month/day fields;
        // the typed getter falls back to them.
        let date = entry.date().expect("typed date");
        match date {
            biblatex::PermissiveType::Typed(date) => {
                assert_eq!(
                    date.value,
                    DateValue::At(Datetime {
                        year: 2003,
                        month: Some(5),
                        day: Some(14),
                        time: None,
                    })
                );
            }
            other => panic!("expected typed date, got {other:?}"),
        }
        assert_eq!(
            entry.get("title").expect("title").format_verbatim(),
            "Dummy & \"quoted\" Example"
        );
        // The escaped characters must have survived the round trip.
        assert!(bibtex.contains("&"));
        assert!(bibtex.contains("quoted"));
    }

    #[test]
    fn test_unique_key() {
        let make = |surname: &str| Biblio {
            authors: vec![author(surname, None)],
            date: Some("2020".to_string()),
            ..Biblio::default()
        };
        let mut used = HashSet::new();
        assert_eq!(unique_key(&make("Smith"), &mut used), "Smith2020");
        assert_eq!(unique_key(&make("Smith"), &mut used), "Smith2020-2");
        assert_eq!(unique_key(&make("Smith"), &mut used), "Smith2020-3");
        assert_eq!(unique_key(&make("Jones"), &mut used), "Jones2020");
    }
}
