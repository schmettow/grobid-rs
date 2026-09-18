//! BibTeX rendering for parsed bibliographic records.
//!
//! GROBID itself can return BibTeX (via the `Accept: application/x-bibtex`
//! header on some services); these helpers instead render the strongly
//! typed [`crate::tei::Biblio`] records client-side, mirroring the
//! client-side converters of the Python GROBID client. They are used by the
//! `pdf2bibtex` and `refs2bibtex` examples.

use std::collections::HashSet;

use crate::tei::{Author, Biblio};

/// Pick a BibTeX entry type based on the available metadata.
///
/// * `article` for journal articles,
/// * `incollection` for chapters in a book,
/// * `inproceedings` for conference series papers,
/// * `techreport` for records with an issuing institution,
/// * `book` for records with a publisher,
/// * `misc` otherwise.
pub fn entry_type(biblio: &Biblio) -> &'static str {
    if biblio.journal.is_some() {
        "article"
    } else if biblio.book_title.is_some() {
        "incollection"
    } else if biblio.series_title.is_some() {
        "inproceedings"
    } else if biblio.institution.is_some() {
        "techreport"
    } else if biblio.publisher.is_some() {
        "book"
    } else {
        "misc"
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

/// Render one record as a BibTeX entry.
pub fn format_entry(key: &str, biblio: &Biblio) -> String {
    let mut lines = vec![format!("@{}{{{key},", entry_type(biblio))];
    if let Some(names) = format_names(&biblio.authors) {
        lines.push(format!("  author = {{{names}}},"));
    }
    if let Some(names) = format_names(&biblio.editors) {
        lines.push(format!("  editor = {{{names}}},"));
    }
    field(&mut lines, "title", &biblio.title);
    field(&mut lines, "booktitle", &biblio.book_title);
    field(&mut lines, "journal", &biblio.journal);
    field(&mut lines, "series", &biblio.series_title);
    field(&mut lines, "year", &year(biblio));
    field(&mut lines, "volume", &biblio.volume);
    field(&mut lines, "number", &biblio.issue);
    field(&mut lines, "pages", &biblio.pages);
    field(&mut lines, "publisher", &biblio.publisher);
    field(&mut lines, "institution", &biblio.institution);
    field(&mut lines, "doi", &biblio.doi);
    field(&mut lines, "url", &biblio.url);
    field(&mut lines, "issn", &biblio.issn);
    field(&mut lines, "note", &biblio.note);
    lines.push("}".to_string());
    lines.join("\n")
}

/// Format authors or editors as `Surname, Given and ...`.
pub fn format_names(names: &[Author]) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    let formatted: Vec<String> = names.iter().filter_map(format_name).collect();
    if formatted.is_empty() {
        None
    } else {
        Some(formatted.join(" and "))
    }
}

fn format_name(author: &Author) -> Option<String> {
    let surname = author.surname.as_deref().unwrap_or_default();
    let given = [author.given_name.as_deref(), author.middle_name.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    if surname.is_empty() {
        author.full_name.clone().filter(|name| !name.is_empty())
    } else if given.is_empty() {
        Some(surname.to_string())
    } else {
        Some(format!("{surname}, {given}"))
    }
}

/// Add a BibTeX field line when the value is present and non-empty.
fn field(lines: &mut Vec<String>, name: &str, value: &Option<String>) {
    if let Some(value) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        lines.push(format!("  {name} = {{{value}}},"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            "article"
        );
        assert_eq!(
            entry_type(&Biblio {
                book_title: Some("X".to_string()),
                ..Biblio::default()
            }),
            "incollection"
        );
        assert_eq!(
            entry_type(&Biblio {
                institution: Some("X".to_string()),
                ..Biblio::default()
            }),
            "techreport"
        );
        assert_eq!(
            entry_type(&Biblio {
                publisher: Some("X".to_string()),
                ..Biblio::default()
            }),
            "book"
        );
        assert_eq!(entry_type(&Biblio::default()), "misc");
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
        assert!(entry.ends_with("\n}"));
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
