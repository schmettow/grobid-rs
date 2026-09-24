//! Client for the GROBID REST API.
//!
//! [`GrobidClient`] talks to a running GROBID service (see
//! <https://grobid.readthedocs.io/>) and returns strongly typed, parsed TEI
//! structures. Requests are made asynchronously with `reqwest`; the client
//! retries requests with exponential backoff when the server responds with
//! HTTP 503 (all processing threads busy) or 429.

use std::path::PathBuf;
use std::time::Duration;

use reqwest::multipart::{Form, Part};
use reqwest::{Client, RequestBuilder, Response, StatusCode};
use url::Url;

use crate::tei::{parse_citation, parse_citation_list, parse_document, Citation, Document};
use crate::Error;

/// The default GROBID server URL (a locally running instance).
pub const DEFAULT_GROBID_URL: &str = "http://localhost:8070";

/// The fulltext processing service (`/api/processFulltextDocument`).
pub const SERVICE_FULLTEXT: &str = "processFulltextDocument";
/// The header processing service (`/api/processHeaderDocument`).
pub const SERVICE_HEADER: &str = "processHeaderDocument";
/// The reference extraction service (`/api/processReferences`).
pub const SERVICE_REFERENCES: &str = "processReferences";
/// The citation parsing service (`/api/processCitation`).
pub const SERVICE_CITATION: &str = "processCitation";
/// The citation list parsing service (`/api/processCitationList`).
pub const SERVICE_CITATION_LIST: &str = "processCitationList";

/// Retry policy for requests that hit a busy GROBID server (HTTP 503 or
/// 429). Retries are spaced with exponential backoff: the delay after the
/// n-th attempt is `initial_delay * multiplier^n`, capped at `max_delay`.
#[derive(Debug, Clone, PartialEq)]
pub struct RetryPolicy {
    /// Total number of attempts, including the first one. Must be at least 1.
    pub max_attempts: usize,
    /// Delay after the first failed attempt.
    pub initial_delay: Duration,
    /// Upper bound for the delay between attempts.
    pub max_delay: Duration,
    /// Backoff growth factor, e.g. `2.0` for exponential backoff.
    pub multiplier: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            initial_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(30),
            multiplier: 2.0,
        }
    }
}

impl RetryPolicy {
    /// The delay to wait after a failed attempt `attempt` (1-based).
    fn delay(&self, attempt: usize) -> Duration {
        let factor = self.multiplier.powi(attempt.saturating_sub(1) as i32);
        let delay = self.initial_delay.mul_f64(factor);
        delay.min(self.max_delay)
    }
}

/// Consolidation level for the document header.
///
/// Header consolidation asks GROBID to complete the extracted metadata with
/// an external lookup (CrossRef or biblio-glutton). The default, as on the
/// GROBID server, is [`HeaderConsolidation::Metadata`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum HeaderConsolidation {
    /// No consolidation (`0`): all metadata comes from the source PDF.
    None,
    /// Consolidate and inject all extra metadata (`1`, server default).
    #[default]
    Metadata,
    /// Consolidate and inject the DOI only (`2`).
    Doi,
    /// Consolidate using only the extracted DOI, if any (`3`).
    DoiOnly,
}

impl HeaderConsolidation {
    fn as_param(self) -> &'static str {
        match self {
            HeaderConsolidation::None => "0",
            HeaderConsolidation::Metadata => "1",
            HeaderConsolidation::Doi => "2",
            HeaderConsolidation::DoiOnly => "3",
        }
    }
}

/// Consolidation level for citations/references.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum CitationConsolidation {
    /// No consolidation (`0`, server default).
    #[default]
    None,
    /// Consolidate and inject all extra metadata (`1`).
    Metadata,
    /// Consolidate and inject the DOI only (`2`).
    Doi,
}

impl CitationConsolidation {
    fn as_param(self) -> &'static str {
        match self {
            CitationConsolidation::None => "0",
            CitationConsolidation::Metadata => "1",
            CitationConsolidation::Doi => "2",
        }
    }
}

/// TEI structures for which PDF coordinates can be requested.
///
/// See <https://grobid.readthedocs.io/en/latest/Coordinates-in-PDF/> for the
/// coordinate system and the `@coords` notation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CoordinateElement {
    /// Reference markers (`ref`).
    Ref,
    /// Bibliographical references (`biblStruct`).
    BiblStruct,
    /// Author names (`persName`).
    PersName,
    /// Figures and tables (`figure`).
    Figure,
    /// Formulas (`formula`).
    Formula,
    /// Section titles (`head`).
    Head,
    /// Sentences (`s`), requires `segment_sentences`.
    Sentence,
    /// Paragraphs (`p`).
    Paragraph,
    /// Footnotes (`note`).
    Note,
    /// Titles (`title`).
    Title,
    /// Affiliations (`affiliation`).
    Affiliation,
}

impl CoordinateElement {
    /// The commonly used set of coordinate elements, as requested by the
    /// Go GROBID client by default.
    pub const COMMON: &'static [CoordinateElement] = &[
        CoordinateElement::Ref,
        CoordinateElement::Figure,
        CoordinateElement::PersName,
        CoordinateElement::Formula,
        CoordinateElement::BiblStruct,
    ];

    fn as_param(self) -> &'static str {
        match self {
            CoordinateElement::Ref => "ref",
            CoordinateElement::BiblStruct => "biblStruct",
            CoordinateElement::PersName => "persName",
            CoordinateElement::Figure => "figure",
            CoordinateElement::Formula => "formula",
            CoordinateElement::Head => "head",
            CoordinateElement::Sentence => "s",
            CoordinateElement::Paragraph => "p",
            CoordinateElement::Note => "note",
            CoordinateElement::Title => "title",
            CoordinateElement::Affiliation => "affiliation",
        }
    }
}

/// Options for a GROBID processing request.
///
/// Defaults follow the GROBID server defaults and the official Python
/// client: header consolidation enabled, citation consolidation and all
/// other extras disabled.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProcessOptions {
    /// Generate unique identifiers for each text component
    /// (`generateIDs=1`).
    pub generate_ids: bool,
    /// Header consolidation level.
    pub consolidate_header: HeaderConsolidation,
    /// Citation consolidation level.
    pub consolidate_citations: CitationConsolidation,
    /// Include the raw reference strings in the result
    /// (`includeRawCitations=1`).
    pub include_raw_citations: bool,
    /// Include the raw affiliation strings in the result
    /// (`includeRawAffiliations=1`).
    pub include_raw_affiliations: bool,
    /// Include the raw copyrights/license strings in the result
    /// (`includeRawCopyrights=1`).
    pub include_raw_copyrights: bool,
    /// TEI structures for which PDF coordinates are added
    /// (`teiCoordinates`, repeated).
    pub tei_coordinates: Vec<CoordinateElement>,
    /// Further segment paragraphs into sentences (`segmentSentences=1`).
    pub segment_sentences: bool,
    /// Structuring flavor to apply, see the GROBID documentation.
    pub flavor: Option<String>,
    /// First PDF page to consider (1-based); `None` starts at the first
    /// page.
    pub start_page: Option<u32>,
    /// Last PDF page to consider (1-based); `None` ends at the last page.
    pub end_page: Option<u32>,
    /// Replace the structured response with a debug dump of the raw CRF
    /// labelling (`debugMode=1`). Note that the response is then *not*
    /// valid TEI and cannot be parsed by the typed methods.
    pub debug_mode: bool,
    /// Restrict the debug dump to the given model names (comma-separated
    /// in the request).
    pub models: Vec<String>,
}

impl ProcessOptions {
    /// Append all option fields to a multipart form.
    fn apply_to(&self, mut form: Form) -> Form {
        form = form.text("consolidateHeader", self.consolidate_header.as_param());
        form = form.text(
            "consolidateCitations",
            self.consolidate_citations.as_param(),
        );
        if self.generate_ids {
            form = form.text("generateIDs", "1");
        }
        if self.include_raw_citations {
            form = form.text("includeRawCitations", "1");
        }
        if self.include_raw_affiliations {
            form = form.text("includeRawAffiliations", "1");
        }
        if self.include_raw_copyrights {
            form = form.text("includeRawCopyrights", "1");
        }
        for element in &self.tei_coordinates {
            form = form.text("teiCoordinates", element.as_param());
        }
        if self.segment_sentences {
            form = form.text("segmentSentences", "1");
        }
        if let Some(flavor) = &self.flavor {
            form = form.text("flavor", flavor.clone());
        }
        if let Some(start) = self.start_page {
            form = form.text("start", start.to_string());
        }
        if let Some(end) = self.end_page {
            form = form.text("end", end.to_string());
        }
        if self.debug_mode {
            form = form.text("debugMode", "1");
        }
        if !self.models.is_empty() {
            form = form.text("models", self.models.join(","));
        }
        form
    }

    /// The citation-related fields for form-urlencoded services.
    fn apply_to_urlencoded(&self, fields: &mut Vec<(String, String)>) {
        if self.consolidate_citations != CitationConsolidation::None {
            fields.push((
                "consolidateCitations".to_string(),
                self.consolidate_citations.as_param().to_string(),
            ));
        }
        if self.include_raw_citations {
            fields.push(("includeRawCitations".to_string(), "1".to_string()));
        }
    }
}

/// A PDF to be processed by GROBID: either a path on disk, or the document
/// bytes with a file name. In-memory inputs avoid a round-trip through the
/// filesystem, e.g. when the PDF was downloaded or comes from an object
/// store.
#[derive(Debug, Clone)]
pub enum PdfInput {
    /// Read the PDF from the given path. The file name of the path is used
    /// as the upload name.
    Path(PathBuf),
    /// Use the given bytes, uploaded under the given file name.
    Data {
        /// The name the PDF is uploaded as, e.g. `paper.pdf`.
        filename: String,
        /// The PDF content.
        data: Vec<u8>,
    },
}

impl From<&str> for PdfInput {
    fn from(path: &str) -> Self {
        PdfInput::Path(PathBuf::from(path))
    }
}

impl From<String> for PdfInput {
    fn from(path: String) -> Self {
        PdfInput::Path(PathBuf::from(path))
    }
}

impl From<&std::path::Path> for PdfInput {
    fn from(path: &std::path::Path) -> Self {
        PdfInput::Path(path.to_path_buf())
    }
}

impl From<PathBuf> for PdfInput {
    fn from(path: PathBuf) -> Self {
        PdfInput::Path(path)
    }
}

impl From<(&str, Vec<u8>)> for PdfInput {
    fn from((filename, data): (&str, Vec<u8>)) -> Self {
        PdfInput::Data {
            filename: filename.to_string(),
            data,
        }
    }
}

impl From<(String, Vec<u8>)> for PdfInput {
    fn from((filename, data): (String, Vec<u8>)) -> Self {
        PdfInput::Data { filename, data }
    }
}

impl PdfInput {
    /// Resolve the input to an upload name and content bytes.
    async fn into_parts(self) -> Result<(String, Vec<u8>), Error> {
        match self {
            PdfInput::Path(path) => {
                let filename = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "document.pdf".to_string());
                let data = tokio::fs::read(&path).await?;
                Ok((filename, data))
            }
            PdfInput::Data { filename, data } => Ok((filename, data)),
        }
    }
}

/// Builder for [`GrobidClient`].
#[derive(Debug, Clone)]
pub struct GrobidClientBuilder {
    base_url: Url,
    http: Option<Client>,
    retry: RetryPolicy,
}

impl GrobidClientBuilder {
    /// Use a custom HTTP client, e.g. with specific timeouts, proxies or
    /// TLS settings.
    pub fn http_client(mut self, client: Client) -> Self {
        self.http = Some(client);
        self
    }

    /// Use a custom retry policy for busy-server (503/429) responses.
    pub fn retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = policy;
        self
    }

    /// Build the client.
    pub fn build(self) -> GrobidClient {
        GrobidClient {
            base_url: self.base_url,
            http: self.http.unwrap_or_default(),
            retry: self.retry,
        }
    }
}

/// A client for the GROBID REST API.
///
/// ```no_run
/// # async fn example() -> Result<(), grobid::Error> {
/// let client = grobid::GrobidClient::new("http://localhost:8070")?;
/// assert!(client.ping().await?);
///
/// let document = client
///     .process_fulltext_document("paper.pdf", &Default::default())
///     .await?;
/// println!("{}", document.header.title.as_deref().unwrap_or("untitled"));
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct GrobidClient {
    base_url: Url,
    http: Client,
    retry: RetryPolicy,
}

impl GrobidClient {
    /// Create a client for a GROBID server with default settings.
    ///
    /// `base_url` may be given with or without a scheme and trailing slash,
    /// e.g. `http://localhost:8070`, `localhost:8070` or
    /// `https://grobid.example.org/`. An optional context path is
    /// preserved.
    pub fn new(base_url: impl AsRef<str>) -> Result<Self, Error> {
        Ok(Self::builder(base_url)?.build())
    }

    /// Create a client builder for customizing the HTTP client and retry
    /// behaviour.
    pub fn builder(base_url: impl AsRef<str>) -> Result<GrobidClientBuilder, Error> {
        let base_url = normalize_base_url(base_url.as_ref())?;
        Ok(GrobidClientBuilder {
            base_url,
            http: None,
            retry: RetryPolicy::default(),
        })
    }

    /// The normalized base URL of the GROBID server.
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// The underlying HTTP client.
    pub fn http_client(&self) -> &Client {
        &self.http
    }

    /// The retry policy used for busy-server responses.
    pub fn retry_policy(&self) -> &RetryPolicy {
        &self.retry
    }

    /// Check that the server is alive (`GET /api/isalive`). Returns `true`
    /// when the server responds successfully.
    pub async fn ping(&self) -> Result<bool, Error> {
        let url = self.api_url("isalive");
        let http = self.http.clone();
        let request_url = url.clone();
        let response = self.request(move || http.get(url.clone())).await?;
        let status = response.status();
        let body = response.text().await?;
        match status {
            StatusCode::OK => Ok(body.trim() == "true"),
            status => Err(Error::http_status(
                status.as_u16(),
                request_url.to_string(),
                body,
            )),
        }
    }

    /// The version of the running GROBID service (`GET /api/version`).
    pub async fn version(&self) -> Result<String, Error> {
        let url = self.api_url("version");
        let http = self.http.clone();
        let request_url = url.clone();
        let response = self.request(move || http.get(url.clone())).await?;
        let status = response.status();
        let body = response.text().await?;
        match status {
            StatusCode::OK => Ok(body.trim().to_string()),
            status => Err(Error::http_status(
                status.as_u16(),
                request_url.to_string(),
                body,
            )),
        }
    }

    /// Process a full document (header, body and references) and return the
    /// parsed TEI (`/api/processFulltextDocument`).
    pub async fn process_fulltext_document(
        &self,
        pdf: impl Into<PdfInput>,
        options: &ProcessOptions,
    ) -> Result<Document, Error> {
        let xml = self
            .process_pdf(SERVICE_FULLTEXT, pdf.into(), options)
            .await?
            .ok_or(Error::NoContent)?;
        parse_document(&xml)
    }

    /// Process the header of a document and return the parsed TEI
    /// (`/api/processHeaderDocument`).
    pub async fn process_header_document(
        &self,
        pdf: impl Into<PdfInput>,
        options: &ProcessOptions,
    ) -> Result<Document, Error> {
        let xml = self
            .process_pdf(SERVICE_HEADER, pdf.into(), options)
            .await?
            .ok_or(Error::NoContent)?;
        parse_document(&xml)
    }

    /// Extract and parse all bibliographic references of a document
    /// (`/api/processReferences`). An HTTP 204 response (nothing extracted)
    /// yields an empty list.
    pub async fn process_references(
        &self,
        pdf: impl Into<PdfInput>,
        options: &ProcessOptions,
    ) -> Result<Vec<Citation>, Error> {
        match self
            .process_pdf(SERVICE_REFERENCES, pdf.into(), options)
            .await?
        {
            Some(xml) => parse_citation_list(&xml),
            None => Ok(Vec::new()),
        }
    }

    /// Parse a single raw bibliographic reference string
    /// (`/api/processCitation`). Returns `None` when GROBID could not
    /// extract any usable citation.
    pub async fn process_citation(
        &self,
        citation: &str,
        options: &ProcessOptions,
    ) -> Result<Option<Citation>, Error> {
        let mut fields = vec![("citations".to_string(), citation.to_string())];
        options.apply_to_urlencoded(&mut fields);
        match self.post_urlencoded(SERVICE_CITATION, fields).await? {
            Some(xml) if !xml.trim().is_empty() => parse_citation(&xml),
            _ => Ok(None),
        }
    }

    /// Parse a list of raw bibliographic reference strings
    /// (`/api/processCitationList`).
    pub async fn process_citation_list(
        &self,
        citations: impl IntoIterator<Item = impl AsRef<str>>,
        options: &ProcessOptions,
    ) -> Result<Vec<Citation>, Error> {
        let mut fields = Vec::new();
        for citation in citations {
            fields.push(("citations".to_string(), citation.as_ref().to_string()));
        }
        options.apply_to_urlencoded(&mut fields);
        match self.post_urlencoded(SERVICE_CITATION_LIST, fields).await? {
            Some(xml) => parse_citation_list(&xml),
            None => Ok(Vec::new()),
        }
    }

    /// Send a PDF to an arbitrary GROBID service (multipart form) and
    /// return the raw response body. Use this for services without a typed
    /// wrapper, e.g. the patent processing services.
    ///
    /// HTTP 204 responses yield [`Error::NoContent`].
    pub async fn process_pdf_raw(
        &self,
        service: &str,
        pdf: impl Into<PdfInput>,
        options: &ProcessOptions,
    ) -> Result<String, Error> {
        match self.process_pdf(service, pdf.into(), options).await? {
            Some(body) => Ok(body),
            None => Err(Error::NoContent),
        }
    }

    /// Send a PDF to a processing service. Returns the response body, or
    /// `None` for HTTP 204.
    async fn process_pdf(
        &self,
        service: &str,
        pdf: PdfInput,
        options: &ProcessOptions,
    ) -> Result<Option<String>, Error> {
        let (filename, data) = pdf.into_parts().await?;
        let url = self.api_url(service);
        let http = self.http.clone();
        let options = options.clone();
        let request_url = url.clone();
        self.send_for_text(request_url, move || {
            let part = Part::bytes(data.clone())
                .file_name(filename.clone())
                .mime_str("application/pdf")
                .expect("fixed MIME type is always valid");
            let form = options.apply_to(Form::new()).part("input", part);
            http.post(url.clone()).multipart(form)
        })
        .await
    }

    /// POST urlencoded fields to `/api/{service}` and return the response
    /// body, or `None` for HTTP 204.
    async fn post_urlencoded(
        &self,
        service: &str,
        fields: Vec<(String, String)>,
    ) -> Result<Option<String>, Error> {
        let url = self.api_url(service);
        let http = self.http.clone();
        let request_url = url.clone();
        self.send_for_text(request_url, move || http.post(url.clone()).form(&fields))
            .await
    }

    /// Send a freshly built request (with busy-server retries), read the
    /// body and translate the status code. The builder closure is invoked
    /// anew for every retry attempt, so request bodies are always
    /// re-encodable.
    async fn send_for_text<F>(&self, url: Url, build: F) -> Result<Option<String>, Error>
    where
        F: Fn() -> RequestBuilder,
    {
        let response = self.request(build).await?;
        let status = response.status();
        let body = response.text().await?;
        match status {
            StatusCode::OK => Ok(Some(body)),
            StatusCode::NO_CONTENT => Ok(None),
            StatusCode::SERVICE_UNAVAILABLE | StatusCode::TOO_MANY_REQUESTS => {
                Err(Error::ServerBusy {
                    attempts: self.retry.max_attempts,
                })
            }
            status => Err(Error::http_status(status.as_u16(), url.to_string(), body)),
        }
    }

    /// Send a request, retrying busy-server responses (503/429) with
    /// exponential backoff until the retry budget is exhausted. The builder
    /// is invoked for every attempt.
    async fn request<F>(&self, build: F) -> Result<Response, Error>
    where
        F: Fn() -> RequestBuilder,
    {
        let mut attempt = 0usize;
        loop {
            attempt += 1;
            let response = build().send().await?;
            let busy = matches!(
                response.status(),
                StatusCode::SERVICE_UNAVAILABLE | StatusCode::TOO_MANY_REQUESTS
            );
            if !busy || attempt >= self.retry.max_attempts {
                return Ok(response);
            }
            tokio::time::sleep(self.retry.delay(attempt)).await;
        }
    }

    /// Build the URL of a service under the `/api/` path.
    fn api_url(&self, service: &str) -> Url {
        // Cannot fail: the constructor only accepts hierarchical http(s)
        // URLs as the base, which always support relative paths.
        self.base_url
            .join(&format!("api/{service}"))
            .expect("base URL always supports relative paths")
    }
}

/// Normalize a user-supplied base URL: add a scheme when missing, require
/// `http`/`https`, and ensure the path ends with a slash so that relative
/// paths can be joined.
fn normalize_base_url(base_url: &str) -> Result<Url, Error> {
    let trimmed = base_url.trim().trim_end_matches('/');
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let mut url = Url::parse(&with_scheme).map_err(|source| Error::InvalidBaseUrl {
        base_url: base_url.to_string(),
        source,
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Error::UnsupportedScheme {
            base_url: base_url.to_string(),
            scheme: url.scheme().to_string(),
        });
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}
