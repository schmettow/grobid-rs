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
use std::path::{Path, PathBuf};

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
///
/// ```
/// use grobid::{bibtex, Biblio};
///
/// let biblio = Biblio {
///     journal: Some("Nature".to_string()),
///     ..Biblio::default()
/// };
/// assert_eq!(bibtex::entry_type(&biblio), biblatex::EntryType::Article);
///
/// let biblio = Biblio {
///     title: Some("A dataset".to_string()),
///     ..Biblio::default()
/// };
/// assert_eq!(bibtex::entry_type(&biblio), biblatex::EntryType::Misc);
/// ```
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
///
/// ```
/// use grobid::{bibtex, Biblio};
///
/// let biblio = Biblio {
///     date: Some("2019-01-30".to_string()),
///     ..Biblio::default()
/// };
/// assert_eq!(bibtex::year(&biblio).as_deref(), Some("2019"));
/// assert_eq!(bibtex::year(&Biblio::default()), None);
/// ```
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
///
/// ```
/// use grobid::{bibtex, Author, Biblio};
///
/// let biblio = Biblio {
///     authors: vec![Author {
///         surname: Some("Milašauskienė".to_string()),
///         ..Author::default()
///     }],
///     date: Some("2003".to_string()),
///     ..Biblio::default()
/// };
/// assert_eq!(bibtex::suggest_key(&biblio), "Milaauskien2003");
/// assert_eq!(bibtex::suggest_key(&Biblio::default()), "ref");
/// ```
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
///
/// ```
/// use std::collections::HashSet;
///
/// use grobid::{bibtex, Author, Biblio};
///
/// let make = |surname: &str| Biblio {
///     authors: vec![Author {
///         surname: Some(surname.to_string()),
///         ..Author::default()
///     }],
///     date: Some("2020".to_string()),
///     ..Biblio::default()
/// };
///
/// let mut used = HashSet::new();
/// assert_eq!(bibtex::unique_key(&make("Smith"), &mut used), "Smith2020");
/// assert_eq!(bibtex::unique_key(&make("Smith"), &mut used), "Smith2020-2");
/// assert_eq!(bibtex::unique_key(&make("Jones"), &mut used), "Jones2020");
/// ```
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

/// Number of title words in a file stem by default.
const DEFAULT_FILE_STEM_TITLE_WORDS: usize = 10;

/// How [`suggest_file_stem_with`] and [`suggest_file_name_with`] format
/// authors, year and title.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileStemStyle {
    /// `Author_Year_Title` — the first author's surname, the year and up to
    /// [`FileStemOptions::title_words`] title words, joined with `_`.
    #[default]
    Compact,
    /// `Author1, Author2, ... - Year - Full title` — all authors, the year
    /// and the complete title, joined with ` - `; every part is stripped of
    /// punctuation, and words stay separated by spaces.
    Full,
}

/// Options for [`suggest_file_stem_with`] and [`suggest_file_name_with`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStemOptions {
    /// How the stem is formatted; see [`FileStemStyle`].
    pub style: FileStemStyle,
    /// Maximum number of title words in [`FileStemStyle::Compact`]; ignored
    /// by [`FileStemStyle::Full`]. `0` omits the title.
    pub title_words: usize,
    /// Keep only ASCII alphanumerics, as [`suggest_key`] does (the default),
    /// or keep all Unicode alphanumerics. Punctuation and path separators
    /// are dropped either way.
    pub ascii_only: bool,
}

impl Default for FileStemOptions {
    fn default() -> Self {
        Self {
            style: FileStemStyle::default(),
            title_words: DEFAULT_FILE_STEM_TITLE_WORDS,
            ascii_only: true,
        }
    }
}

/// Suggest a file stem `Author_Year_Title` from the first author's surname,
/// the publication year and up to ten title words, e.g.
/// `Kahle_2000_The_Barc_model_for_continuous_variables`.
///
/// Missing parts are omitted, and words that contain no alphanumeric
/// character are dropped. Returns `None` when neither author, year nor title
/// is known. Non-ASCII letters are dropped, as in [`suggest_key`]; use
/// [`suggest_file_stem_with`] to keep them.
///
/// ```
/// use grobid::{bibtex, Author, Biblio};
///
/// let biblio = Biblio {
///     authors: vec![Author {
///         surname: Some("Kahle".to_string()),
///         ..Author::default()
///     }],
///     date: Some("2000-03-01".to_string()),
///     title: Some("The Barc model for continuous variables".to_string()),
///     ..Biblio::default()
/// };
/// assert_eq!(
///     bibtex::suggest_file_stem(&biblio).as_deref(),
///     Some("Kahle_2000_The_Barc_model_for_continuous_variables")
/// );
/// ```
pub fn suggest_file_stem(biblio: &Biblio) -> Option<String> {
    suggest_file_stem_with(biblio, &FileStemOptions::default())
}

/// Like [`suggest_file_stem`], with explicit [`FileStemOptions`].
///
/// ```
/// use grobid::{
///     bibtex::{self, FileStemOptions, FileStemStyle},
///     Author, Biblio,
/// };
///
/// let biblio = Biblio {
///     authors: vec![Author {
///         surname: Some("Milašauskienė".to_string()),
///         ..Author::default()
///     }],
///     date: Some("2003".to_string()),
///     title: Some("Über die Müdigkeit".to_string()),
///     ..Biblio::default()
/// };
///
/// // The compact default drops non-ASCII letters (as `suggest_key` does).
/// assert_eq!(
///     bibtex::suggest_file_stem_with(&biblio, &FileStemOptions::default()).as_deref(),
///     Some("Milaauskien_2003_ber_die_Mdigkeit")
/// );
///
/// // The full style lists all authors, keeps the whole title and uses
/// // ` - ` separators; Unicode letters can be kept.
/// let full = FileStemOptions {
///     style: FileStemStyle::Full,
///     ascii_only: false,
///     ..Default::default()
/// };
/// assert_eq!(
///     bibtex::suggest_file_stem_with(&biblio, &full).as_deref(),
///     Some("Milašauskienė - 2003 - Über die Müdigkeit")
/// );
/// ```
pub fn suggest_file_stem_with(biblio: &Biblio, options: &FileStemOptions) -> Option<String> {
    match options.style {
        FileStemStyle::Compact => compact_file_stem(biblio, options),
        FileStemStyle::Full => full_file_stem(biblio, options),
    }
}

/// The underscore-joined `Author_Year_Title` stem.
fn compact_file_stem(biblio: &Biblio, options: &FileStemOptions) -> Option<String> {
    let author = biblio
        .authors
        .first()
        .and_then(|author| author.surname.as_deref())
        .map(|surname| sanitize_part(surname, options.ascii_only))
        .unwrap_or_default();
    let year = year(biblio).unwrap_or_default();
    let title = biblio
        .title
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
        .take(options.title_words)
        .map(|word| sanitize_part(word, options.ascii_only))
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    let stem = [author, year, title]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    if stem.is_empty() {
        None
    } else {
        Some(stem)
    }
}

/// The space-joined `Author1, Author2, ... - Year - Full title` stem.
fn full_file_stem(biblio: &Biblio, options: &FileStemOptions) -> Option<String> {
    let authors = biblio
        .authors
        .iter()
        .filter_map(|author| {
            author
                .surname
                .as_deref()
                .or(author.full_name.as_deref())
                .map(|name| sanitize_words(name, options.ascii_only))
                .filter(|name| !name.is_empty())
        })
        .collect::<Vec<_>>()
        .join(", ");
    let year = year(biblio).unwrap_or_default();
    let title = biblio
        .title
        .as_deref()
        .map(|title| sanitize_words(title, options.ascii_only))
        .unwrap_or_default();
    let stem = [authors, year, title]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" - ");
    if stem.is_empty() {
        None
    } else {
        Some(stem)
    }
}

/// Suggest a file name for `path`: its directory and extension with the
/// stem from [`suggest_file_stem`]. Returns `None` when no stem can be
/// suggested, in which case the file should keep its name.
///
/// ```
/// use std::path::{Path, PathBuf};
///
/// use grobid::{bibtex, Biblio};
///
/// let biblio = Biblio {
///     date: Some("2020-01-30".to_string()),
///     title: Some("--- A *real* title".to_string()),
///     ..Biblio::default()
/// };
/// assert_eq!(
///     bibtex::suggest_file_name(Path::new("papers/paper.PDF"), &biblio),
///     Some(PathBuf::from("papers/2020_A_real_title.PDF"))
/// );
/// ```
pub fn suggest_file_name(path: impl AsRef<Path>, biblio: &Biblio) -> Option<PathBuf> {
    suggest_file_name_with(path, biblio, &FileStemOptions::default())
}

/// Like [`suggest_file_name`], with explicit [`FileStemOptions`].
pub fn suggest_file_name_with(
    path: impl AsRef<Path>,
    biblio: &Biblio,
    options: &FileStemOptions,
) -> Option<PathBuf> {
    let path = path.as_ref();
    let stem = suggest_file_stem_with(biblio, options)?;
    let mut target = path.with_file_name(stem);
    if let Some(extension) = path.extension() {
        target.set_extension(extension);
    }
    Some(target)
}

/// The first free variant of `path`, extended with a `-2`, `-3`, ... suffix
/// before the file extension while the path already exists. Use it to turn
/// several suggestions for the same name into collision-free names.
///
/// ```
/// use std::path::PathBuf;
///
/// use grobid::bibtex;
///
/// // Nothing exists at this path, so it is returned unchanged.
/// assert_eq!(
///     bibtex::unique_path("/no/such/file.pdf"),
///     PathBuf::from("/no/such/file.pdf")
/// );
/// ```
pub fn unique_path(path: impl Into<PathBuf>) -> PathBuf {
    let target = path.into();
    let mut candidate = target.clone();
    let mut suffix = 2usize;
    while candidate.exists() {
        candidate = with_suffix(&target, suffix);
        suffix += 1;
    }
    candidate
}

/// Insert a `-<suffix>` marker before the file extension.
fn with_suffix(path: &Path, suffix: usize) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    match path.extension() {
        Some(extension) => {
            path.with_file_name(format!("{stem}-{suffix}.{}", extension.to_string_lossy()))
        }
        None => path.with_file_name(format!("{stem}-{suffix}")),
    }
}

/// Filter a stem part down to alphanumerics (ASCII-only or Unicode).
fn sanitize_part(text: &str, ascii_only: bool) -> String {
    if ascii_only {
        text.chars().filter(char::is_ascii_alphanumeric).collect()
    } else {
        text.chars().filter(|c| c.is_alphanumeric()).collect()
    }
}

/// Strip punctuation from every whitespace-separated word and join them
/// with single spaces; words without any alphanumeric character disappear.
fn sanitize_words(text: &str, ascii_only: bool) -> String {
    text.split_whitespace()
        .map(|word| sanitize_part(word, ascii_only))
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Convert a bibliographic record into a typed BibLaTeX entry.
///
/// Authors and editors become typed person lists, the GROBID date (year,
/// year-month or year-month-day) becomes a typed date, and the remaining
/// fields become plain text values. The returned entry can be serialized
/// with [`Entry::to_bibtex_string`], inspected with the typed getters, or
/// inserted into a [`biblatex::Bibliography`].
///
/// ```
/// use grobid::{bibtex, Author, Biblio};
///
/// let biblio = Biblio {
///     authors: vec![Author {
///         given_name: Some("Brewster".to_string()),
///         surname: Some("Kahle".to_string()),
///         ..Author::default()
///     }],
///     title: Some("Dummy Example File".to_string()),
///     date: Some("2000-03-01".to_string()),
///     ..Biblio::default()
/// };
///
/// let entry = bibtex::to_entry("kahle2000", &biblio);
///
/// // The entry is typed, not just text: fields can be read back through
/// // the biblatex getters.
/// let authors = entry.get_as::<Vec<biblatex::Person>>("author").unwrap();
/// assert_eq!(authors[0].name, "Kahle");
/// assert_eq!(authors[0].given_name, "Brewster");
/// println!("{}", entry.to_bibtex_string().unwrap());
/// ```
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
///
/// ```
/// use grobid::{bibtex, Author, Biblio};
///
/// let biblio = Biblio {
///     authors: vec![Author {
///         given_name: Some("Brewster".to_string()),
///         surname: Some("Kahle".to_string()),
///         ..Author::default()
///     }],
///     title: Some("Dummy Example File".to_string()),
///     journal: Some("Journal of Fake News".to_string()),
///     date: Some("2000".to_string()),
///     ..Biblio::default()
/// };
///
/// let entry = bibtex::format_entry("kahle2000", &biblio);
/// assert!(entry.starts_with("@article{kahle2000,\n"));
/// assert!(entry.contains("author = {Kahle, Brewster},"));
/// assert!(entry.contains("journal = {Journal of Fake News},"));
/// ```
///
/// # Panics
///
/// This function does not panic for any input: entries built from typed
/// values in [`to_entry`] always serialize, and unparseable dates are
/// emitted as a literal `date = {...}` field rather than causing an error.
/// The internal assertion on that invariant exists so that a future
/// regression surfaces as a panic instead of a silently truncated entry.
pub fn format_entry(key: &str, biblio: &Biblio) -> String {
    render(to_entry(key, biblio))
}

/// Render one record as a BibTeX entry with a `file` field.
///
/// Like [`format_entry`], but records `file` (typically the path of the
/// document the metadata was extracted from) in the entry's `file` field,
/// so that reference managers can open the document. The path is written
/// verbatim; relative paths are kept as given.
///
/// ```
/// use grobid::{bibtex, Author, Biblio};
///
/// let biblio = Biblio {
///     authors: vec![Author {
///         given_name: Some("Brewster".to_string()),
///         surname: Some("Kahle".to_string()),
///         ..Author::default()
///     }],
///     title: Some("Dummy Example File".to_string()),
///     date: Some("2000".to_string()),
///     ..Biblio::default()
/// };
///
/// let entry = bibtex::format_entry_with_file("kahle2000", &biblio, "/papers/kahle2000.pdf");
/// assert!(entry.contains("file = {/papers/kahle2000.pdf},"));
/// ```
///
/// # Panics
///
/// See [`format_entry`]: entries built from typed values always serialize.
pub fn format_entry_with_file(key: &str, biblio: &Biblio, file: impl AsRef<Path>) -> String {
    let mut entry = to_entry(key, biblio);
    entry.set(
        "file",
        vec![Spanned::detached(Chunk::Normal(
            file.as_ref().to_string_lossy().into_owned(),
        ))],
    );
    render(entry)
}

/// Serialize an entry, treating a serialization failure as a bug.
fn render(entry: Entry) -> String {
    match entry.to_bibtex_string() {
        Ok(entry) => entry,
        // Unreachable in practice; prefer a panicking expect over silently
        // returning a truncated entry, because a bug here must be noticed.
        Err(err) => unreachable!("entries built from typed values always serialize: {err}"),
    }
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
    fn test_format_entry_with_file() {
        let mut biblio = Biblio::default();
        biblio.authors.push(author("Kahle", Some("Brewster")));
        biblio.title = Some("Dummy Example File".to_string());
        biblio.date = Some("2000".to_string());

        let entry = format_entry_with_file("kahle2000", &biblio, "/papers/kahle2000.pdf");
        assert!(entry.contains("file = {/papers/kahle2000.pdf},"), "{entry}");
        // The field is read back verbatim, e.g. by a reference manager.
        let bibliography = biblatex::Bibliography::parse(&entry).expect("parse output");
        let parsed = bibliography.get("kahle2000").expect("entry by key");
        assert_eq!(
            parsed.get("file").expect("file").format_verbatim(),
            "/papers/kahle2000.pdf"
        );
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

    #[test]
    fn test_suggest_file_stem() {
        // The default keeps only ASCII, exactly like `suggest_key`.
        let biblio = Biblio {
            authors: vec![author("Milašauskienė", None)],
            date: Some("2003".to_string()),
            title: Some(
                "One two three four five six seven eight nine ten eleven twelve".to_string(),
            ),
            ..Biblio::default()
        };
        assert_eq!(
            suggest_file_stem(&biblio).as_deref(),
            Some("Milaauskien_2003_One_two_three_four_five_six_seven_eight_nine_ten")
        );

        // Missing parts are omitted; punctuation-only words are dropped.
        let biblio = Biblio {
            date: Some("2020-01-30".to_string()),
            title: Some("--- A *real* title".to_string()),
            ..Biblio::default()
        };
        assert_eq!(
            suggest_file_stem(&biblio).as_deref(),
            Some("2020_A_real_title")
        );
        assert_eq!(suggest_file_stem(&Biblio::default()), None);
    }

    #[test]
    fn test_suggest_file_stem_options() {
        let biblio = Biblio {
            authors: vec![author("Milašauskienė", None)],
            date: Some("2003".to_string()),
            title: Some("Über die Müdigkeit".to_string()),
            ..Biblio::default()
        };
        let unicode = FileStemOptions {
            ascii_only: false,
            ..FileStemOptions::default()
        };
        assert_eq!(
            suggest_file_stem_with(&biblio, &unicode).as_deref(),
            Some("Milašauskienė_2003_Über_die_Müdigkeit")
        );
        let no_title = FileStemOptions {
            title_words: 0,
            ..FileStemOptions::default()
        };
        assert_eq!(
            suggest_file_stem_with(&biblio, &no_title).as_deref(),
            Some("Milaauskien_2003")
        );
    }

    #[test]
    fn test_suggest_file_stem_full_style() {
        let biblio = Biblio {
            authors: vec![
                author("Kahle", None),
                author("Smith", Some("Jane")),
                // Without a surname, the full name is used.
                Author {
                    full_name: Some("No Surname".to_string()),
                    ..Author::default()
                },
                author("Milašauskienė", None),
            ],
            date: Some("2000-03-01".to_string()),
            title: Some(
                "The B.A.R.C. model: for continuous variables — an introduction!".to_string(),
            ),
            ..Biblio::default()
        };
        // All authors, the full title, punctuation stripped, Unicode kept.
        let full_unicode = FileStemOptions {
            style: FileStemStyle::Full,
            ascii_only: false,
            ..FileStemOptions::default()
        };
        assert_eq!(
            suggest_file_stem_with(&biblio, &full_unicode).as_deref(),
            Some(
                "Kahle, Smith, No Surname, Milašauskienė - 2000 - \
                 The BARC model for continuous variables an introduction"
            )
        );
        // The ASCII policy drops diacritics, as in `suggest_key`.
        let full_ascii = FileStemOptions {
            style: FileStemStyle::Full,
            ..FileStemOptions::default()
        };
        assert_eq!(
            suggest_file_stem_with(&biblio, &full_ascii).as_deref(),
            Some(
                "Kahle, Smith, No Surname, Milaauskien - 2000 - \
                 The BARC model for continuous variables an introduction"
            )
        );
    }

    #[test]
    fn test_full_style_omits_missing_parts_and_keeps_long_titles() {
        let biblio = Biblio {
            date: Some("2020".to_string()),
            // Eleven words: the full style must not truncate.
            title: Some("One two three four five six seven eight nine ten eleven".to_string()),
            ..Biblio::default()
        };
        let full = FileStemOptions {
            style: FileStemStyle::Full,
            title_words: 3, // ignored by the full style
            ..FileStemOptions::default()
        };
        assert_eq!(
            suggest_file_stem_with(&biblio, &full).as_deref(),
            Some("2020 - One two three four five six seven eight nine ten eleven")
        );
        assert_eq!(suggest_file_stem_with(&Biblio::default(), &full), None);
    }

    #[test]
    fn test_suggest_file_name_keeps_directory_and_extension() {
        let biblio = Biblio {
            date: Some("2020-01-30".to_string()),
            title: Some("--- A *real* title".to_string()),
            ..Biblio::default()
        };
        assert_eq!(
            suggest_file_name(Path::new("papers/paper.PDF"), &biblio),
            Some(PathBuf::from("papers/2020_A_real_title.PDF"))
        );
        assert_eq!(
            suggest_file_name(Path::new("paper.pdf"), &Biblio::default()),
            None
        );
    }

    #[test]
    fn test_unique_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("Smith_2020_Title.pdf");
        assert_eq!(unique_path(target.clone()), target);

        std::fs::write(&target, b"pdf").expect("write");
        let second = unique_path(target.clone());
        assert_eq!(second.file_name().unwrap(), "Smith_2020_Title-2.pdf");

        std::fs::write(&second, b"pdf").expect("write");
        let third = unique_path(target);
        assert_eq!(third.file_name().unwrap(), "Smith_2020_Title-3.pdf");
    }
}
