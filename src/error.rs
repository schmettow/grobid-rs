//! Error types for the grobid crate.

/// Errors produced by the grobid client and TEI parser.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The given base URL is not a valid URL.
    #[error("invalid base URL {base_url:?}: {source}")]
    InvalidBaseUrl {
        /// The invalid base URL as given by the caller.
        base_url: String,
        /// The underlying URL parse error.
        #[source]
        source: url::ParseError,
    },

    /// The given base URL is valid, but uses a scheme other than `http` or
    /// `https`.
    #[error(
        "unsupported URL scheme {scheme:?} in {base_url:?}: \
         GROBID servers are reached over http or https"
    )]
    UnsupportedScheme {
        /// The base URL as given by the caller.
        base_url: String,
        /// The unsupported scheme.
        scheme: String,
    },

    /// An I/O error occurred, e.g. while reading a PDF from disk.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// An HTTP transport error occurred.
    #[error("HTTP transport error: {0}")]
    Http(#[from] reqwest::Error),

    /// The GROBID server returned an unexpected HTTP status code.
    #[error("GROBID server at {url} returned HTTP {status}: {message}")]
    HttpStatus {
        /// HTTP status code.
        status: u16,
        /// The full URL that was requested.
        url: String,
        /// The response body, truncated to a reasonable length.
        message: String,
    },

    /// The GROBID server kept responding with 503/429 (all processing
    /// threads busy) until the retry budget was exhausted.
    #[error("GROBID server is busy (HTTP 503/429) after {attempts} attempts")]
    ServerBusy {
        /// Number of attempts made.
        attempts: usize,
    },

    /// The GROBID server completed the request but produced no content
    /// (HTTP 204).
    #[error("GROBID server returned no content (HTTP 204)")]
    NoContent,

    /// The response body is not well-formed XML.
    #[error("invalid XML: {0}")]
    Xml(#[from] roxmltree::Error),

    /// The XML is well-formed, but does not look like a GROBID TEI document.
    #[error("invalid TEI document: {0}")]
    InvalidDocument(#[from] InvalidDocument),

    /// A malformed `@coords` attribute value was encountered.
    #[error(
        "invalid @coords value {value:?}: expected \"page,x,y,width,height\" \
         bounding boxes separated by ';'"
    )]
    InvalidCoords {
        /// The malformed value.
        value: String,
    },
}

/// Reasons why a well-formed XML document is not a usable GROBID TEI
/// document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum InvalidDocument {
    /// The root element is not `<TEI>`.
    #[error("root element is not <TEI>")]
    NotTei,
    /// The document has no `<teiHeader>`.
    #[error("missing <teiHeader>")]
    MissingTeiHeader,
    /// The header has no `<appInfo><application>` element, which identifies
    /// the GROBID version that produced the document.
    #[error("missing <appInfo><application>")]
    MissingApplication,
}

impl Error {
    /// Construct an HTTP status error from a status code and a response body.
    pub(crate) fn http_status(status: u16, url: impl Into<String>, mut message: String) -> Self {
        const MAX_MESSAGE_LEN: usize = 500;
        if message.len() > MAX_MESSAGE_LEN {
            message.truncate(MAX_MESSAGE_LEN);
            message.push('…');
        }
        let message = message.trim().to_string();
        Self::HttpStatus {
            status,
            url: url.into(),
            message,
        }
    }
}
