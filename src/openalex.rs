//! Second-tier reference completion against [OpenAlex](https://openalex.org/).
//!
//! GROBID extracts references from PDFs and can already consolidate them
//! against CrossRef while processing (see
//! [`CitationConsolidation`](crate::CitationConsolidation)). This module adds
//! an optional second tier on top: a [`Completer`] looks a reference up on
//! OpenAlex — by DOI when one was parsed, otherwise by title search — and
//! fills the fields GROBID could not extract from the matching work.
//!
//! Existing fields are never overwritten: completion only fills gaps. A
//! search result is accepted only when title, publication year and first
//! author are compatible with the parsed reference, so a lookup that cannot
//! be verified leaves the reference unchanged.
//!
//! # Feature
//!
//! The module is available with the `openalex` cargo feature, which adds the
//! [`openalex`](https://crates.io/crates/openalex) crate for its typed `Work`
//! model. That crate (0.2.2) predates several OpenAlex API schema changes and
//! its blocking HTTP helpers no longer deserialize current responses; this
//! module therefore fetches with the async `reqwest` client that is already a
//! dependency, repairs the known schema drift, and then deserializes into the
//! crate's model. Lookups go to `https://api.openalex.org` by default and
//! require HTTPS access to it; [`Completer::with_base_url`] points the
//! completer at a mirror instead.
//!
//! # Example
//!
//! ```no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use grobid::openalex::Completer;
//! use grobid::Biblio;
//!
//! // A reference as parsed by GROBID: it has a DOI, but no journal data.
//! let biblio = Biblio {
//!     doi: Some("10.7717/peerj.4375".to_string()),
//!     ..Biblio::default()
//! };
//!
//! let completer = Completer::new();
//! if let Some(completion) = completer.complete(&biblio).await? {
//!     println!("matched {:?}", completion.openalex_id);
//!     let completed: Biblio = completion.biblio;
//!     println!("journal: {:?}", completed.journal);
//! }
//! # Ok(())
//! # }
//! ```

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use url::Url;

use ::openalex::api_entities::common_types::DehydratedSource;
use ::openalex::api_entities::work::WorkResponse;
use ::openalex::Work;

use crate::bibtex;
use crate::tei::{Author, Biblio};

/// The public OpenAlex API, used by [`Completer::new`].
pub const DEFAULT_OPENALEX_URL: &str = "https://api.openalex.org";

/// Number of OpenAlex search results considered for a title match.
const SEARCH_LIMIT: u32 = 10;

/// Titles shorter than this are not searched for, as too little text would
/// make a false-positive match likely.
const MIN_TITLE_LEN: usize = 8;

/// The shorter of two normalized titles must be at least this long for a
/// containment match (e.g. when only one side carries the subtitle).
const CONTAINMENT_MIN_TITLE_LEN: usize = 20;

/// Response bodies in error messages are truncated to this length.
const MAX_ERROR_MESSAGE_LEN: usize = 500;

/// How a reference was matched against OpenAlex.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    /// The parsed DOI was looked up directly.
    Doi,
    /// The parsed title was searched and a candidate accepted.
    Title,
}

/// A reference that was matched against OpenAlex.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Completion {
    /// The completed record. Fields that were already present are unchanged.
    pub biblio: Biblio,
    /// The OpenAlex work ID, e.g. `https://openalex.org/W2741809807`.
    pub openalex_id: String,
    /// How the work was matched.
    pub matched_by: MatchKind,
}

/// Errors from the OpenAlex completion tier.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The base URL is not a valid URL.
    #[error("invalid OpenAlex base URL {base_url:?}: {source}")]
    InvalidBaseUrl {
        /// The invalid base URL as given by the caller.
        base_url: String,
        /// The underlying URL parse error.
        #[source]
        source: url::ParseError,
    },

    /// The base URL is valid, but uses a scheme other than `http` or
    /// `https`.
    #[error("unsupported URL scheme {scheme:?} in {base_url:?}")]
    UnsupportedScheme {
        /// The base URL as given by the caller.
        base_url: String,
        /// The unsupported scheme.
        scheme: String,
    },

    /// An HTTP transport error occurred.
    #[error("OpenAlex HTTP transport error: {0}")]
    Http(#[from] reqwest::Error),

    /// The OpenAlex API returned an unexpected HTTP status code.
    #[error("OpenAlex API at {url} returned HTTP {status}: {message}")]
    HttpStatus {
        /// HTTP status code.
        status: u16,
        /// The full URL that was requested.
        url: String,
        /// The response body, truncated to a reasonable length.
        message: String,
    },

    /// The response body could not be parsed into the `openalex` crate's
    /// model.
    #[error("could not parse OpenAlex response from {url}: {source}")]
    Parse {
        /// The full URL that was requested.
        url: String,
        /// The underlying JSON error.
        #[source]
        source: serde_json::Error,
    },
}

/// Completes references against OpenAlex.
///
/// A completer holds the base URL and a connection pool, so it should be
/// created once and reused; cloning it is cheap.
#[derive(Debug, Clone)]
pub struct Completer {
    base_url: Url,
    http: reqwest::Client,
}

impl Completer {
    /// Create a completer for the public OpenAlex API
    /// ([`DEFAULT_OPENALEX_URL`]).
    ///
    /// # Panics
    ///
    /// Never in practice: the built-in URL is a valid, constant URL (a unit
    /// test asserts this). Use [`Completer::with_base_url`] when the URL is
    /// not known to be valid.
    pub fn new() -> Self {
        Self {
            base_url: Url::parse(DEFAULT_OPENALEX_URL).expect("built-in OpenAlex URL is valid"),
            http: reqwest::Client::new(),
        }
    }

    /// Create a completer for another OpenAlex-compatible base URL, e.g. a
    /// mirror or a local test server.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidBaseUrl`] when `base_url` is not a valid URL
    /// and [`Error::UnsupportedScheme`] when it does not use `http` or
    /// `https`.
    pub fn with_base_url(base_url: impl AsRef<str>) -> Result<Self, Error> {
        let base_url = base_url.as_ref();
        let parsed = Url::parse(base_url).map_err(|source| Error::InvalidBaseUrl {
            base_url: base_url.to_string(),
            source,
        })?;
        match parsed.scheme() {
            "http" | "https" => Ok(Self {
                base_url: parsed,
                http: reqwest::Client::new(),
            }),
            scheme => Err(Error::UnsupportedScheme {
                base_url: base_url.to_string(),
                scheme: scheme.to_string(),
            }),
        }
    }

    /// The base URL requests are sent to.
    pub fn base_url(&self) -> &str {
        self.base_url.as_str()
    }

    /// Look a reference up on OpenAlex and fill in its missing fields.
    ///
    /// The reference is matched by DOI when one is present; otherwise its
    /// parsed title is searched. A title match is accepted only when the
    /// titles are compatible and the publication year and first author (when
    /// both are known on both sides) do not contradict each other.
    ///
    /// Fields that are already set on `biblio` are never overwritten. Authors
    /// are only taken over when no author was parsed at all, and container
    /// titles (`journal`, `book_title`) are only taken over when the OpenAlex
    /// work type agrees, so that a book chapter does not become a journal
    /// article and a preprint does not become a book.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] when the OpenAlex request fails. A reference that
    /// cannot be matched, or that carries neither a DOI nor a usable title,
    /// is not an error: the function then returns `Ok(None)`.
    pub async fn complete(&self, biblio: &Biblio) -> Result<Option<Completion>, Error> {
        let Some((work, matched_by)) = self.find_work(biblio).await? else {
            return Ok(None);
        };
        Ok(Some(Completion {
            biblio: merge(biblio, &work),
            openalex_id: work.id.clone(),
            matched_by,
        }))
    }

    /// Fetch the OpenAlex work matching a reference.
    ///
    /// A parsed DOI is looked up first; when OpenAlex does not know it (or no
    /// DOI was parsed), the title is searched.
    async fn find_work(&self, biblio: &Biblio) -> Result<Option<(Work, MatchKind)>, Error> {
        if let Some(doi) = biblio.doi.as_deref().and_then(normalize_doi) {
            if let Some(work) = self.lookup_by_doi(&doi).await? {
                return Ok(Some((work, MatchKind::Doi)));
            }
        }

        let Some(title) = biblio
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| title.chars().count() >= MIN_TITLE_LEN)
        else {
            return Ok(None);
        };
        let candidates = self.search_by_title(title).await?;
        Ok(select_match(biblio, candidates).map(|work| (work, MatchKind::Title)))
    }

    /// Look a DOI up with the single-work endpoint.
    ///
    /// Returns `Ok(None)` when OpenAlex does not know the DOI; that is not an
    /// error, because the parsed title may still find the work.
    async fn lookup_by_doi(&self, doi: &str) -> Result<Option<Work>, Error> {
        let mut url = self.works_url();
        url.path_segments_mut()
            .expect("http(s) URLs can be a base")
            .push(&format!("doi:{doi}"));
        let response = self.send(&url).await?;
        match response.status() {
            reqwest::StatusCode::OK => {}
            reqwest::StatusCode::NOT_FOUND => return Ok(None),
            _ => return Err(self.status_error(&url, response).await),
        }
        let value = self.json_body(&url, response).await?;
        Ok(Some(parse_work(&url, value)?))
    }

    /// Search works by title, most cited first.
    ///
    /// Most cited first so that the canonical record of a work is considered
    /// even when OpenAlex holds several versions of it; [`select_match`]
    /// then filters out incompatible candidates.
    async fn search_by_title(&self, title: &str) -> Result<Vec<Work>, Error> {
        let mut url = self.works_url();
        url.query_pairs_mut()
            .append_pair("filter", &format!("title.search:{title}"))
            .append_pair("per-page", &SEARCH_LIMIT.to_string())
            .append_pair("sort", "cited_by_count:desc");
        let response = self.send(&url).await?;
        if !response.status().is_success() {
            return Err(self.status_error(&url, response).await);
        }
        let value = self.json_body(&url, response).await?;
        Ok(parse_work_response(&url, value)?.results)
    }

    /// The `/works` endpoint of the configured base URL.
    fn works_url(&self) -> Url {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .expect("http(s) URLs can be a base")
            .pop_if_empty()
            .push("works");
        url
    }

    /// Send a GET request, identifying this client.
    async fn send(&self, url: &Url) -> Result<reqwest::Response, Error> {
        let request = self.http.get(url.clone()).header(
            reqwest::header::USER_AGENT,
            concat!("grobid-rs/", env!("CARGO_PKG_VERSION")),
        );
        Ok(request.send().await?)
    }

    /// Read a successful response body and parse it as JSON.
    async fn json_body(&self, url: &Url, response: reqwest::Response) -> Result<Value, Error> {
        let text = response.text().await?;
        serde_json::from_str(&text).map_err(|source| Error::Parse {
            url: url.to_string(),
            source,
        })
    }

    /// Build an error from an unsuccessful response, including the truncated
    /// body as the message.
    async fn status_error(&self, url: &Url, response: reqwest::Response) -> Error {
        let status = response.status().as_u16();
        let message = response.text().await.unwrap_or_default();
        Error::HttpStatus {
            status,
            url: url.to_string(),
            message: truncate_message(message),
        }
    }
}

impl Default for Completer {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a single work, repairing the known schema drift first.
fn parse_work(url: &Url, mut value: Value) -> Result<Work, Error> {
    sanitize_work(&mut value);
    serde_json::from_value(value).map_err(|source| Error::Parse {
        url: url.to_string(),
        source,
    })
}

/// Parse a work list response, repairing the known schema drift first.
fn parse_work_response(url: &Url, mut value: Value) -> Result<WorkResponse, Error> {
    sanitize_work(&mut value);
    serde_json::from_value(value).map_err(|source| Error::Parse {
        url: url.to_string(),
        source,
    })
}

/// Repair the schema drift between the current OpenAlex API and the model of
/// the `openalex` crate (0.2.2), which predates several changes:
///
/// * required fields that the API no longer returns (`cited_by_api_url`,
///   `grants`, `type_crossref`) are inserted with empty values,
/// * `null` values for required booleans, strings and arrays are replaced by
///   `false`, `""` and `[]`,
/// * required objects that are `null` (`cited_by_percentile_year`,
///   `open_access`) are replaced by their defaults.
///
/// Unknown extra fields are ignored during deserialization and need no
/// repair.
fn sanitize_work(value: &mut Value) {
    match value {
        Value::Object(object) => {
            // Required arrays, which may be absent or `null`.
            for (key, default) in [
                ("grants", json!([])),
                ("host_organization_lineage", json!([])),
                ("issn", json!([])),
                ("institutions", json!([])),
            ] {
                if object.get(key).is_none_or(Value::is_null) {
                    object.insert(key.to_string(), default);
                }
            }
            // Required strings, which may be absent or `null`.
            for (key, default) in [
                ("cited_by_api_url", json!("")),
                ("type_crossref", json!("")),
                ("provenance", json!("")),
                ("display_name", json!("")),
                ("ror", json!("")),
                ("descriptor_ui", json!("")),
                ("descriptor_name", json!("")),
                ("qualifier_ui", json!("")),
            ] {
                if object.get(key).is_none_or(Value::is_null) {
                    object.insert(key.to_string(), default);
                }
            }
            // `deserialize_null_default` accepts `null`, but the field is
            // still required to be present.
            object
                .entry("abstract_inverted_index")
                .or_insert(Value::Null);
            for key in [
                "is_oa",
                "is_accepted",
                "is_published",
                "is_in_doaj",
                "is_major_topic",
                "has_fulltext",
                "is_paratext",
                "is_retracted",
                "any_repository_has_fulltext",
            ] {
                if object.get(key).is_some_and(Value::is_null) {
                    object.insert(key.to_string(), Value::Bool(false));
                }
            }
            if object
                .get("cited_by_percentile_year")
                .is_some_and(Value::is_null)
            {
                object.insert(
                    "cited_by_percentile_year".to_string(),
                    json!({ "min": 0, "max": 0 }),
                );
            }
            if object.get("open_access").is_some_and(Value::is_null) {
                object.insert(
                    "open_access".to_string(),
                    json!({
                        "is_oa": false,
                        "oa_status": "closed",
                        "oa_url": null,
                        "any_repository_has_fulltext": false
                    }),
                );
            }
            for (key, child) in object.iter_mut() {
                // The abstract inverted index is a free-form word ->
                // positions map, not an entity: its keys are not fields and
                // must not be repaired.
                if key != "abstract_inverted_index" {
                    sanitize_work(child);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                sanitize_work(item);
            }
        }
        _ => {}
    }
}

/// Trim a response body and truncate it to a reasonable length, cutting at a
/// character boundary.
fn truncate_message(mut message: String) -> String {
    if message.len() > MAX_ERROR_MESSAGE_LEN {
        let mut end = MAX_ERROR_MESSAGE_LEN;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        message.push('…');
    }
    message.trim().to_string()
}

/// Pick the best compatible search result.
///
/// Candidates are compatible when their title matches and their year and
/// first author do not contradict the parsed reference. Among compatible
/// candidates, an exact title beats a containment match, an exact year beats
/// a one-year difference, and the most cited work wins (the canonical record
/// of a work usually has the most citations, e.g. when OpenAlex holds several
/// versions of it).
fn select_match(biblio: &Biblio, candidates: Vec<Work>) -> Option<Work> {
    let query = biblio.title.as_deref()?;
    let parsed_title = normalize_title(query);
    let parsed_year = parsed_year(biblio);
    candidates
        .into_iter()
        .filter_map(|work| {
            let title = normalize_title(work_title(&work));
            if title.is_empty() || !titles_match(&parsed_title, &title) {
                return None;
            }
            if !year_compatible(biblio, &work) || !authors_compatible(biblio, &work) {
                return None;
            }
            let exact_title = u8::from(title == parsed_title);
            let exact_year =
                u8::from(work.publication_year != 0 && parsed_year == Some(work.publication_year));
            Some((exact_title, exact_year, work.cited_by_count, work))
        })
        .max_by_key(|(exact_title, exact_year, cited_by_count, _)| {
            (*exact_title, *exact_year, *cited_by_count)
        })
        .map(|(_, _, _, work)| work)
}

/// The display title of a work.
fn work_title(work: &Work) -> &str {
    work.title
        .as_deref()
        .or(work.display_name.as_deref())
        .unwrap_or_default()
}

/// Whether a parsed title and an OpenAlex title refer to the same work.
///
/// Titles are compared case- and punctuation-insensitively. When one side
/// carries extra text (typically a subtitle), the shorter title must still be
/// long enough for a containment match.
fn titles_match(parsed: &str, candidate: &str) -> bool {
    let parsed = normalize_title(parsed);
    let candidate = normalize_title(candidate);
    if parsed.is_empty() || candidate.is_empty() {
        return false;
    }
    if parsed == candidate {
        return true;
    }
    let (longer, shorter) = if parsed.len() >= candidate.len() {
        (parsed.as_str(), candidate.as_str())
    } else {
        (candidate.as_str(), parsed.as_str())
    };
    shorter.chars().count() >= CONTAINMENT_MIN_TITLE_LEN && longer.contains(shorter)
}

/// Lowercase a string, keep alphanumerics and collapse everything else into
/// single spaces, for case- and punctuation-insensitive comparisons.
fn normalize_title(raw: &str) -> String {
    let mut normalized = String::with_capacity(raw.len());
    let mut pending_space = false;
    for c in raw.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            if pending_space && !normalized.is_empty() {
                normalized.push(' ');
            }
            pending_space = false;
            normalized.push(c);
        } else {
            pending_space = true;
        }
    }
    normalized
}

/// The parsed publication year, when there is a usable one.
fn parsed_year(biblio: &Biblio) -> Option<u32> {
    bibtex::year(biblio).and_then(|year| year.parse::<u32>().ok())
}

/// Whether the parsed publication year is compatible with the work's year.
///
/// A one-year difference is tolerated (e.g. online-first vs issue year). An
/// unknown year on either side is compatible with anything.
fn year_compatible(biblio: &Biblio, work: &Work) -> bool {
    let Some(parsed) = parsed_year(biblio) else {
        return true;
    };
    work.publication_year == 0 || parsed.abs_diff(work.publication_year) <= 1
}

/// Whether the parsed first author is compatible with the work's first author.
///
/// Only the last whitespace-separated word of each name is compared, which
/// tolerates missing initials and name particles (`van der Berg` vs `Berg`).
/// An unknown name on either side is compatible with anything.
fn authors_compatible(biblio: &Biblio, work: &Work) -> bool {
    let Some(candidate) = work
        .authorships
        .first()
        .map(|authorship| authorship.author.display_name.as_str())
    else {
        return true;
    };
    let Some(parsed) = biblio
        .authors
        .first()
        .and_then(|author| author.surname.as_deref().or(author.full_name.as_deref()))
    else {
        return true;
    };
    match (name_token(parsed), name_token(candidate)) {
        (Some(parsed), Some(candidate)) => parsed == candidate,
        _ => true,
    }
}

/// The last whitespace-separated word of a name, normalized for comparison.
fn name_token(name: &str) -> Option<String> {
    normalize_title(name)
        .split_whitespace()
        .last()
        .map(str::to_string)
}

/// Strip a URL or `doi:` prefix from a DOI and trim whitespace.
fn normalize_doi(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    let stripped = [
        "https://doi.org/",
        "http://doi.org/",
        "https://dx.doi.org/",
        "http://dx.doi.org/",
        "doi:",
    ]
    .iter()
    .find_map(|prefix| lower.starts_with(prefix).then(|| &trimmed[prefix.len()..]))
    .unwrap_or(trimmed)
    .trim();
    (!stripped.is_empty()).then(|| stripped.to_string())
}

/// Merge a matched work into a reference, filling only missing fields.
fn merge(biblio: &Biblio, work: &Work) -> Biblio {
    let mut merged = biblio.clone();

    fill(&mut merged.title, nonempty(Some(work_title(work))));
    let date = work_date(work);
    fill(&mut merged.date, date.as_deref());
    if merged.authors.is_empty() {
        merged.authors = work_authors(work);
    }

    fill_source_fields(&mut merged, work, work_source(work));

    let volume = work.biblio.volume.as_deref();
    fill(&mut merged.volume, volume);
    let issue = work.biblio.issue.as_deref();
    fill(&mut merged.issue, issue);
    let first_page = work.biblio.first_page.as_deref();
    fill(&mut merged.first_page, first_page);
    let last_page = work.biblio.last_page.as_deref();
    fill(&mut merged.last_page, last_page);
    let pages = work_pages(work);
    fill(&mut merged.pages, pages.as_deref());

    let doi = work.doi.as_deref().and_then(normalize_doi);
    fill(&mut merged.doi, doi.as_deref());
    fill(&mut merged.pmid, work.ids.pmid.as_deref());
    fill(&mut merged.url, work_url(work));

    merged
}

/// The primary source of a work, falling back to the first location that has
/// one.
fn work_source(work: &Work) -> Option<&DehydratedSource> {
    work.primary_location
        .as_ref()
        .and_then(|location| location.source.as_ref())
        .or_else(|| {
            work.locations
                .iter()
                .find_map(|location| location.source.as_ref())
        })
}

/// Fill container, publisher and ISSN from the work's source.
///
/// The work and source types decide where the source belongs: a journal
/// source becomes the journal, the source of a book chapter or conference
/// paper becomes the book title, and a repository (e.g. a preprint server)
/// contributes no container at all. Publisher and institution are filled only
/// for publication-like work types, so a repository host does not turn a
/// preprint into a `@book` entry.
fn fill_source_fields(merged: &mut Biblio, work: &Work, source: Option<&DehydratedSource>) {
    let Some(source) = source else {
        return;
    };
    match source.source_type.as_deref() {
        Some("journal") => {
            fill(
                &mut merged.journal,
                nonempty(Some(source.display_name.as_str())),
            );
            fill(&mut merged.issn, source_issn(source));
        }
        Some("repository" | "metadata" | "other") => {}
        _ => match work.work_type.as_str() {
            "article" | "review" | "editorial" | "letter" => {
                fill(
                    &mut merged.journal,
                    nonempty(Some(source.display_name.as_str())),
                );
                fill(&mut merged.issn, source_issn(source));
            }
            "book-chapter" | "proceedings-article" => {
                fill(
                    &mut merged.book_title,
                    nonempty(Some(source.display_name.as_str())),
                );
            }
            _ => {}
        },
    }
    match work.work_type.as_str() {
        "article"
        | "review"
        | "editorial"
        | "letter"
        | "book"
        | "book-chapter"
        | "proceedings-article" => {
            fill(
                &mut merged.publisher,
                nonempty(source.host_organization_name.as_deref()),
            );
        }
        "report" | "dissertation" => {
            fill(
                &mut merged.institution,
                nonempty(source.host_organization_name.as_deref()),
            );
        }
        _ => {}
    }
}

/// The first ISSN of a source, preferring the print ISSN list over the
/// linking ISSN.
fn source_issn(source: &DehydratedSource) -> Option<&str> {
    source
        .issn
        .iter()
        .map(String::as_str)
        .find(|issn| !issn.trim().is_empty())
        .or_else(|| {
            source
                .issn_l
                .as_deref()
                .filter(|issn| !issn.trim().is_empty())
        })
}

/// The publication date of a work, falling back to its year.
fn work_date(work: &Work) -> Option<String> {
    if let Some(date) = nonempty(Some(work.publication_date.as_str())) {
        return Some(date.to_string());
    }
    (work.publication_year > 0).then(|| work.publication_year.to_string())
}

/// The page range of a work, from its first and last page.
fn work_pages(work: &Work) -> Option<String> {
    let first = nonempty(work.biblio.first_page.as_deref());
    let last = nonempty(work.biblio.last_page.as_deref());
    match (first, last) {
        (Some(first), Some(last)) if first != last => Some(format!("{first}-{last}")),
        (Some(page), _) | (_, Some(page)) => Some(page.to_string()),
        (None, None) => None,
    }
}

/// The landing page of a work, falling back to the first location and the
/// OpenAlex page.
fn work_url(work: &Work) -> Option<&str> {
    work.primary_location
        .as_ref()
        .and_then(|location| nonempty(location.landing_page_url.as_deref()))
        .or_else(|| {
            work.locations
                .iter()
                .find_map(|location| nonempty(location.landing_page_url.as_deref()))
        })
        .or(Some(work.id.as_str()))
}

/// Convert OpenAlex authorships into authors.
///
/// OpenAlex only provides a display name, so the last whitespace-separated
/// word becomes the surname and the rest the given name; the full display
/// name is kept as well. Two-word names are treated as given name plus
/// surname, and single-word names as surnames.
fn work_authors(work: &Work) -> Vec<Author> {
    work.authorships
        .iter()
        .filter_map(|authorship| {
            let name = authorship.author.display_name.trim();
            if name.is_empty() {
                return None;
            }
            let (given_name, surname) = split_name(name);
            Some(Author {
                full_name: Some(name.to_string()),
                given_name,
                surname,
                orcid: nonempty(authorship.author.orcid.as_deref()).map(str::to_string),
                ..Author::default()
            })
        })
        .collect()
}

/// Split a display name into given name and surname at the last whitespace.
fn split_name(name: &str) -> (Option<String>, Option<String>) {
    match name.rsplit_once(char::is_whitespace) {
        Some((given, surname)) => (
            nonempty(Some(given)).map(str::to_string),
            nonempty(Some(surname)).map(str::to_string),
        ),
        None => (None, Some(name.to_string())),
    }
}

/// Set an optional field from a value, when the field is not already set and
/// the value is non-empty.
fn fill(target: &mut Option<String>, value: Option<&str>) {
    if nonempty(target.as_deref()).is_some() {
        return;
    }
    if let Some(value) = nonempty(value) {
        *target = Some(value.to_string());
    }
}

/// Trim a value and treat an empty result as absent.
fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    /// A work JSON document with all required fields set; not built with the
    /// `json!` macro to keep macro recursion shallow.
    const BASE_WORK: &str = r#"
    {
        "abstract_inverted_index": null,
        "authorships": [],
        "biblio": {},
        "cited_by_api_url": "https://api.openalex.org/works?filter=cites:W1",
        "cited_by_count": 0,
        "concepts": [],
        "corresponding_author_ids": [],
        "corresponding_institution_ids": [],
        "countries_distinct_count": 0,
        "counts_by_year": [],
        "created_date": "2020-01-01",
        "display_name": null,
        "doi": null,
        "fulltext_origin": null,
        "grants": [],
        "has_fulltext": false,
        "id": "https://openalex.org/W1",
        "ids": {
            "openalex": "https://openalex.org/W1"
        },
        "indexed_in": [],
        "institutions_distinct_count": 0,
        "is_paratext": false,
        "is_retracted": false,
        "keywords": [],
        "language": "en",
        "license": null,
        "locations": [],
        "locations_count": 0,
        "mesh": [],
        "ngrams_url": null,
        "open_access": {
            "is_oa": false,
            "oa_status": "closed",
            "oa_url": null,
            "any_repository_has_fulltext": false
        },
        "primary_location": null,
        "primary_topic": null,
        "publication_date": "2020-01-01",
        "publication_year": 2020,
        "referenced_works": [],
        "related_works": [],
        "sustainable_development_goals": [],
        "title": "Example",
        "topics": [],
        "type": "article",
        "type_crossref": "journal-article",
        "updated_date": "2020-01-02",
        "cited_by_percentile_year": {
            "min": 0,
            "max": 0
        },
        "fwci": null,
        "referenced_works_count": 0
    }
    "#;

    /// Build a work from a fixture with all required fields set, merging the
    /// given overrides on top.
    fn work_fixture(overrides: Value) -> Work {
        let mut base: Value = serde_json::from_str(BASE_WORK).expect("base fixture is valid JSON");
        {
            let base = base.as_object_mut().expect("fixture is an object");
            let overrides = overrides.as_object().expect("overrides are an object");
            for (key, value) in overrides {
                base.insert(key.clone(), value.clone());
            }
        }
        serde_json::from_value(base).expect("fixture deserializes into a work")
    }

    fn source(display_name: &str, source_type: &str, host: &str) -> Value {
        json!({
            "display_name": display_name,
            "host_organization_lineage": [],
            "host_organization_name": host,
            "id": "https://openalex.org/S1",
            "is_in_doaj": false,
            "is_oa": false,
            "issn": ["1234-5678"],
            "type": source_type
        })
    }

    fn authorship(display_name: &str) -> Value {
        json!({
            "author_position": "first",
            "author": { "display_name": display_name },
            "institutions": []
        })
    }

    #[test]
    fn test_default_completer() {
        let completer = Completer::new();
        assert_eq!(completer.base_url(), "https://api.openalex.org/");
        assert_eq!(
            completer.works_url().as_str(),
            "https://api.openalex.org/works"
        );
    }

    #[test]
    fn test_with_base_url_validation() {
        assert!(Completer::with_base_url("not a url").is_err());
        assert!(matches!(
            Completer::with_base_url("ftp://example.org"),
            Err(Error::UnsupportedScheme { .. })
        ));
        let completer =
            Completer::with_base_url("http://127.0.0.1:8080/mirror").expect("valid base url");
        assert_eq!(
            completer.works_url().as_str(),
            "http://127.0.0.1:8080/mirror/works"
        );
    }

    #[test]
    fn test_sanitize_repairs_schema_drift() {
        let mut value: Value = serde_json::from_str(BASE_WORK).expect("base fixture is valid JSON");
        {
            let object = value.as_object_mut().expect("fixture is an object");
            object.remove("cited_by_api_url");
            object.remove("grants");
            object.remove("type_crossref");
            object.insert(
                "apc_list".to_string(),
                json!({ "value": 1, "currency": "USD", "value_usd": 1 }),
            );
            object.insert(
                "locations".to_string(),
                json!([{
                    "is_oa": true,
                    "is_published": null,
                    "source": {
                        "id": "https://openalex.org/S1",
                        "display_name": "Example Journal",
                        "is_in_doaj": false,
                        "is_oa": false,
                        "issn": null
                    }
                }]),
            );
        }
        sanitize_work(&mut value);
        let work: Work = serde_json::from_value(value).expect("sanitized work parses");

        assert!(work.grants.is_empty());
        assert_eq!(work.cited_by_api_url, "");
        assert_eq!(work.type_crossref, "");
        assert_eq!(work.apc_list.expect("apc list").provenance, "");
        assert!(!work.locations[0].is_published);
        let source = work.locations[0].source.as_ref().expect("source");
        assert!(source.issn.is_empty());
        assert!(source.host_organization_lineage.is_empty());
    }

    #[test]
    fn test_normalize_doi() {
        assert_eq!(normalize_doi(" 10.1/x ").as_deref(), Some("10.1/x"));
        assert_eq!(
            normalize_doi("https://doi.org/10.1/X").as_deref(),
            Some("10.1/X")
        );
        assert_eq!(
            normalize_doi("http://dx.doi.org/10.1/x").as_deref(),
            Some("10.1/x")
        );
        assert_eq!(normalize_doi("doi:10.1/x").as_deref(), Some("10.1/x"));
        assert_eq!(normalize_doi("   "), None);
    }

    #[test]
    fn test_titles_match() {
        assert!(titles_match("The Barc model", "the barc model!"));
        assert!(titles_match(
            "The Barc model for continuous variables: a tutorial",
            "The Barc model for continuous variables"
        ));
        assert!(!titles_match("The Barc model", "A different paper"));
        // Shorter than the containment threshold.
        assert!(!titles_match(
            "Deep learning",
            "Deep learning for everything"
        ));
    }

    #[test]
    fn test_year_and_author_compatibility() {
        let biblio = Biblio {
            date: Some("2000".to_string()),
            authors: vec![Author {
                surname: Some("Kahle".to_string()),
                ..Author::default()
            }],
            ..Biblio::default()
        };
        let same = work_fixture(json!({
            "publication_year": 2000,
            "authorships": [authorship("Brewster Kahle")]
        }));
        assert!(year_compatible(&biblio, &same));
        assert!(authors_compatible(&biblio, &same));

        // A one-year difference (online first vs issue) is tolerated.
        let close = work_fixture(json!({ "publication_year": 2001 }));
        assert!(year_compatible(&biblio, &close));

        let wrong_year = work_fixture(json!({ "publication_year": 1980 }));
        assert!(!year_compatible(&biblio, &wrong_year));

        // Name particles are ignored by comparing the last words.
        let particles = Biblio {
            authors: vec![Author {
                surname: Some("van der Berg".to_string()),
                ..Author::default()
            }],
            ..Biblio::default()
        };
        let particle = work_fixture(json!({
            "authorships": [authorship("Cornelis van der Berg")]
        }));
        assert!(authors_compatible(&particles, &particle));

        // A different person is not compatible.
        assert!(!authors_compatible(&biblio, &particle));

        // Unknown names on either side are compatible.
        let unknown = work_fixture(json!({ "authorships": [authorship("")] }));
        assert!(authors_compatible(&Biblio::default(), &unknown));
        assert!(year_compatible(&Biblio::default(), &wrong_year));
    }

    #[test]
    fn test_work_authors_splits_display_names() {
        let work = work_fixture(json!({
            "authorships": [
                authorship("Brewster Kahle"),
                authorship("Cornelis van der Berg"),
                authorship("Aristotle")
            ]
        }));
        let authors = work_authors(&work);
        assert_eq!(authors.len(), 3);
        assert_eq!(authors[0].given_name.as_deref(), Some("Brewster"));
        assert_eq!(authors[0].surname.as_deref(), Some("Kahle"));
        assert_eq!(authors[0].full_name.as_deref(), Some("Brewster Kahle"));
        assert_eq!(authors[1].given_name.as_deref(), Some("Cornelis van der"));
        assert_eq!(authors[1].surname.as_deref(), Some("Berg"));
        assert_eq!(authors[2].given_name, None);
        assert_eq!(authors[2].surname.as_deref(), Some("Aristotle"));
    }

    #[test]
    fn test_merge_fills_missing_fields() {
        let work = work_fixture(json!({
            "title": "The Barc model for continuous variables",
            "doi": "https://doi.org/10.1002/example",
            "publication_date": "2000-03-01",
            "publication_year": 2000,
            "ids": { "openalex": "https://openalex.org/W1", "pmid": "12345678" },
            "authorships": [{
                "author_position": "first",
                "author": {
                    "display_name": "Brewster Kahle",
                    "orcid": "https://orcid.org/0000-0001-2345-6789"
                },
                "institutions": []
            }],
            "biblio": {
                "volume": "18",
                "issue": "1",
                "first_page": "17",
                "last_page": "27"
            },
            "primary_location": {
                "is_oa": false,
                "landing_page_url": "https://example.org/article",
                "source": source("Cell Biochemistry and Function", "journal", "Wiley")
            }
        }));
        let biblio = Biblio {
            title: Some("The Barc model for continuous variables".to_string()),
            ..Biblio::default()
        };
        let merged = merge(&biblio, &work);

        assert_eq!(merged.date.as_deref(), Some("2000-03-01"));
        assert_eq!(
            merged.journal.as_deref(),
            Some("Cell Biochemistry and Function")
        );
        assert_eq!(merged.publisher.as_deref(), Some("Wiley"));
        assert_eq!(merged.issn.as_deref(), Some("1234-5678"));
        assert_eq!(merged.volume.as_deref(), Some("18"));
        assert_eq!(merged.issue.as_deref(), Some("1"));
        assert_eq!(merged.first_page.as_deref(), Some("17"));
        assert_eq!(merged.last_page.as_deref(), Some("27"));
        assert_eq!(merged.pages.as_deref(), Some("17-27"));
        assert_eq!(merged.doi.as_deref(), Some("10.1002/example"));
        assert_eq!(merged.pmid.as_deref(), Some("12345678"));
        assert_eq!(merged.url.as_deref(), Some("https://example.org/article"));
        assert_eq!(merged.authors.len(), 1);
        assert_eq!(
            merged.authors[0].orcid.as_deref(),
            Some("https://orcid.org/0000-0001-2345-6789")
        );
    }

    #[test]
    fn test_merge_keeps_existing_fields() {
        let work = work_fixture(json!({
            "title": "OpenAlex title",
            "doi": "https://doi.org/10.1002/example",
            "publication_year": 2000,
            "authorships": [authorship("Brewster Kahle")],
            "biblio": { "volume": "18" },
            "primary_location": {
                "is_oa": false,
                "source": source("OpenAlex Journal", "journal", "Wiley")
            }
        }));
        let biblio = Biblio {
            title: Some("Parsed title".to_string()),
            journal: Some("Parsed Journal".to_string()),
            volume: Some("1".to_string()),
            authors: vec![Author {
                surname: Some("Parsed".to_string()),
                ..Author::default()
            }],
            ..Biblio::default()
        };
        let merged = merge(&biblio, &work);

        assert_eq!(merged.title.as_deref(), Some("Parsed title"));
        assert_eq!(merged.journal.as_deref(), Some("Parsed Journal"));
        assert_eq!(merged.volume.as_deref(), Some("1"));
        assert_eq!(merged.authors[0].surname.as_deref(), Some("Parsed"));
        // Missing fields are still filled.
        assert_eq!(merged.doi.as_deref(), Some("10.1002/example"));
    }

    #[test]
    fn test_merge_container_types() {
        let chapter = work_fixture(json!({
            "type": "book-chapter",
            "primary_location": {
                "is_oa": false,
                "source": source("Handbook of Examples", "book series", "Springer")
            }
        }));
        let merged = merge(&Biblio::default(), &chapter);
        assert_eq!(merged.book_title.as_deref(), Some("Handbook of Examples"));
        assert_eq!(merged.journal, None);
        assert_eq!(merged.publisher.as_deref(), Some("Springer"));

        let preprint = work_fixture(json!({
            "type": "preprint",
            "primary_location": {
                "is_oa": true,
                "source": source("arXiv", "repository", "Cornell University")
            }
        }));
        let merged = merge(&Biblio::default(), &preprint);
        assert_eq!(merged.journal, None);
        assert_eq!(merged.book_title, None);
        assert_eq!(merged.publisher, None);
        assert_eq!(merged.institution, None);

        let report = work_fixture(json!({
            "type": "report",
            "primary_location": {
                "is_oa": false,
                "source": source("Reports", "repository", "Some Institute")
            }
        }));
        let merged = merge(&Biblio::default(), &report);
        assert_eq!(merged.institution.as_deref(), Some("Some Institute"));
    }

    #[test]
    fn test_select_match_filters_incompatible_candidates() {
        let biblio = Biblio {
            title: Some("The Barc model for continuous variables".to_string()),
            date: Some("2000".to_string()),
            authors: vec![Author {
                surname: Some("Kahle".to_string()),
                ..Author::default()
            }],
            ..Biblio::default()
        };
        let matching = |overrides: Value| {
            let mut base = json!({
                "title": "The Barc model for continuous variables",
                "publication_year": 2000,
                "authorships": [authorship("Brewster Kahle")]
            });
            {
                let base = base.as_object_mut().expect("match fixture is an object");
                for (key, value) in overrides.as_object().expect("overrides are an object") {
                    base.insert(key.clone(), value.clone());
                }
            }
            work_fixture(base)
        };

        let wrong_title = matching(json!({ "title": "A different paper entirely" }));
        assert!(select_match(&biblio, vec![wrong_title]).is_none());

        let wrong_year = matching(json!({ "publication_year": 1980 }));
        assert!(select_match(&biblio, vec![wrong_year]).is_none());

        let wrong_author = matching(json!({ "authorships": [authorship("Someone Else")] }));
        assert!(select_match(&biblio, vec![wrong_author]).is_none());

        // Incompatible candidates are skipped; a compatible one is taken.
        let wrong = matching(json!({ "title": "A different paper entirely" }));
        let right = matching(json!({}));
        let selected = select_match(&biblio, vec![wrong, right]).expect("second candidate matches");
        assert_eq!(
            selected.title.as_deref(),
            Some("The Barc model for continuous variables")
        );

        // Among compatible candidates, the exact year wins over a nearby one.
        let off_by_one = matching(json!({ "publication_year": 2001, "cited_by_count": 9999 }));
        let exact = matching(json!({ "publication_year": 2000, "cited_by_count": 1 }));
        let selected = select_match(&biblio, vec![off_by_one, exact]).expect("match");
        assert_eq!(selected.publication_year, 2000);

        // And the most cited work wins (typically the canonical record).
        let rarely_cited = matching(json!({ "cited_by_count": 1 }));
        let canonical = matching(json!({ "cited_by_count": 100 }));
        let selected = select_match(&biblio, vec![rarely_cited, canonical]).expect("match");
        assert_eq!(selected.cited_by_count, 100);
    }

    #[test]
    fn test_work_pages() {
        let work = work_fixture(json!({
            "biblio": { "first_page": "17", "last_page": "27" }
        }));
        assert_eq!(work_pages(&work).as_deref(), Some("17-27"));
        let work = work_fixture(json!({ "biblio": { "first_page": "17" } }));
        assert_eq!(work_pages(&work).as_deref(), Some("17"));
        let work = work_fixture(json!({ "biblio": {} }));
        assert_eq!(work_pages(&work), None);
    }

    #[test]
    fn test_sanitize_leaves_the_abstract_inverted_index_alone() {
        let mut value: Value = serde_json::from_str(BASE_WORK).expect("base fixture is valid JSON");
        value.as_object_mut().expect("object").insert(
            "abstract_inverted_index".to_string(),
            json!({ "issn": [0], "grants": [1], "a": [2] }),
        );
        sanitize_work(&mut value);
        let work: Work = serde_json::from_value(value).expect("sanitized work parses");
        assert_eq!(work.abstract_inverted_index.len(), 3);
        assert_eq!(work.abstract_inverted_index["issn"], vec![0]);
        assert_eq!(work.abstract_inverted_index["grants"], vec![1]);
    }

    #[tokio::test]
    async fn test_complete_without_doi_or_title_skips_lookup() {
        // No DOI and no title: nothing to look up, and no request is made.
        let result = Completer::new().complete(&Biblio::default()).await;
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn test_truncate_message() {
        assert_eq!(truncate_message("  short  ".to_string()), "short");
        // Three-byte characters exercise cutting at a character boundary.
        let long = "€".repeat(200);
        let truncated = truncate_message(long);
        assert!(truncated.ends_with('…'));
        assert!(truncated.len() <= MAX_ERROR_MESSAGE_LEN + '…'.len_utf8());
    }
}
