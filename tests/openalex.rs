//! Tests for the OpenAlex completion tier, exercised against a minimal
//! in-process mock server. The mock serves canned OpenAlex responses, so no
//! live API is required.

#![cfg(feature = "openalex")]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use grobid::openalex::{Completer, Error, MatchKind};
use grobid::{Author, Biblio};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A canned HTTP response of the mock server.
struct MockResponse {
    status: u16,
    body: String,
}

impl MockResponse {
    fn ok(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            body: body.into(),
        }
    }

    fn status(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
        }
    }
}

/// Spawn a mock OpenAlex server. `handler` maps the request target (path and
/// query) to a response; the targets are recorded so that tests can assert on
/// them (assertions in the spawned task would be silently swallowed).
async fn spawn_mock<F>(handler: F) -> (SocketAddr, Arc<Mutex<Vec<String>>>)
where
    F: Fn(&str) -> MockResponse + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            loop {
                let n = stream.read(&mut tmp).await.expect("read");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let head = String::from_utf8_lossy(&buf);
            let target = head
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string();
            recorded.lock().expect("lock").push(target.clone());

            let response = handler(&target);
            let head = format!(
                "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.status,
                reason(response.status),
                response.body.len()
            );
            stream.write_all(head.as_bytes()).await.expect("write head");
            stream
                .write_all(response.body.as_bytes())
                .await
                .expect("write body");
            let _ = stream.shutdown().await;
        }
    });
    (addr, requests)
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Error",
    }
}

fn completer(addr: SocketAddr) -> Completer {
    Completer::with_base_url(format!("http://{addr}")).expect("completer")
}

/// A work shaped like a current OpenAlex response, with the fields
/// completion uses and plenty of fields it ignores.
const WORK: &str = r#"{
    "id": "https://openalex.org/W2741809807",
    "doi": "https://doi.org/10.7717/peerj.4375",
    "title": "The state of OA: a large-scale analysis of the prevalence and impact of Open Access articles",
    "display_name": "The state of OA: a large-scale analysis of the prevalence and impact of Open Access articles",
    "publication_year": 2018,
    "publication_date": "2018-02-13",
    "type": "article",
    "ids": {
        "openalex": "https://openalex.org/W2741809807",
        "doi": "https://doi.org/10.7717/peerj.4375",
        "mag": 2741809807,
        "pmid": "https://pubmed.ncbi.nlm.nih.gov/29456894"
    },
    "authorships": [
        {
            "author_position": "first",
            "author": {
                "id": "https://openalex.org/A5085615206",
                "display_name": "Heather Piwowar",
                "orcid": "https://orcid.org/0000-0003-1613-5981"
            },
            "institutions": []
        },
        {
            "author_position": "last",
            "author": {
                "id": "https://openalex.org/A5101467598",
                "display_name": "Jason Priem"
            },
            "institutions": []
        }
    ],
    "biblio": {
        "volume": "6",
        "issue": null,
        "first_page": "e4375",
        "last_page": "e4375"
    },
    "primary_location": {
        "is_oa": true,
        "is_published": null,
        "landing_page_url": "https://doi.org/10.7717/peerj.4375",
        "source": {
            "id": "https://openalex.org/S1983995261",
            "display_name": "PeerJ",
            "host_organization_lineage": [],
            "host_organization_name": "PeerJ",
            "is_in_doaj": true,
            "is_oa": true,
            "issn": ["2167-8359"],
            "type": "journal"
        }
    },
    "locations": [],
    "locations_count": 1,
    "apc_list": { "value": 1395, "currency": "USD", "value_usd": 1395 },
    "has_fulltext": true,
    "is_paratext": false,
    "is_retracted": false,
    "language": "en",
    "license": "cc-by",
    "open_access": {
        "is_oa": true,
        "oa_status": "gold",
        "oa_url": "https://peerj.com/articles/4375.pdf",
        "any_repository_has_fulltext": true
    },
    "cited_by_count": 356,
    "corresponding_author_ids": [],
    "corresponding_institution_ids": [],
    "countries_distinct_count": 2,
    "counts_by_year": [],
    "institutions_distinct_count": 2,
    "referenced_works": [],
    "referenced_works_count": 0,
    "related_works": [],
    "mesh": [],
    "keywords": [],
    "concepts": [],
    "topics": [],
    "sustainable_development_goals": [],
    "indexed_in": ["crossref"],
    "created_date": "2018-02-13",
    "updated_date": "2026-01-01",
    "cited_by_percentile_year": { "min": 99, "max": 100 },
    "fwci": 10.5
}"#;

/// A wrong candidate with the same title as [`WORK`], but a wrong year and a
/// wrong first author.
const WRONG_CANDIDATE: &str = r#"{
    "id": "https://openalex.org/W2",
    "title": "The state of OA: a large-scale analysis of the prevalence and impact of Open Access articles",
    "publication_year": 1990,
    "authorships": [{"author": {"display_name": "Someone Else"}}]
}"#;

/// A work whose title contains a comma, the case that produced an HTTP 400
/// from OpenAlex's filter parser (the comma separates filters).
const COMMA_WORK: &str = r#"{
    "id": "https://openalex.org/W63174745",
    "title": "Sexual selection, sensory systems and sensory exploitation.",
    "publication_year": 1990,
    "authorships": [{"author": {"display_name": "Michael J. Ryan"}}]
}"#;

/// A work whose title carries a stray `*` from the parsed reference, the
/// case that produced the wildcard HTTP 400 for the stemmed title search.
const WILDCARD_WORK: &str = r#"{
    "id": "https://openalex.org/W2145765358",
    "title": "Newborns' preferential tracking of face-like stimuli and its subsequent decline",
    "publication_year": 1991,
    "authorships": [{"author": {"display_name": "Mark H. Johnson"}}]
}"#;

fn search_response(works: &str) -> String {
    format!(
        r#"{{"meta": {{"count": 1, "db_response_time_ms": 5, "page": 1, "per_page": 5, "groups_count": null}}, "results": [{works}]}}"#
    )
}

#[tokio::test]
async fn test_complete_by_doi() {
    let (addr, requests) = spawn_mock(|_| MockResponse::ok(WORK)).await;
    let biblio = Biblio {
        doi: Some("10.7717/peerj.4375".to_string()),
        ..Biblio::default()
    };

    let completion = completer(addr)
        .complete(&biblio)
        .await
        .expect("request succeeds")
        .expect("match");

    assert_eq!(completion.matched_by, MatchKind::Doi);
    assert_eq!(completion.openalex_id, "https://openalex.org/W2741809807");
    assert_eq!(
        completion.biblio.title.as_deref(),
        Some("The state of OA: a large-scale analysis of the prevalence and impact of Open Access articles")
    );
    assert_eq!(completion.biblio.journal.as_deref(), Some("PeerJ"));
    assert_eq!(completion.biblio.publisher.as_deref(), Some("PeerJ"));
    assert_eq!(completion.biblio.date.as_deref(), Some("2018-02-13"));
    assert_eq!(completion.biblio.volume.as_deref(), Some("6"));
    assert_eq!(completion.biblio.pages.as_deref(), Some("e4375"));
    assert_eq!(completion.biblio.doi.as_deref(), Some("10.7717/peerj.4375"));
    assert_eq!(
        completion.biblio.pmid.as_deref(),
        Some("https://pubmed.ncbi.nlm.nih.gov/29456894")
    );
    assert_eq!(
        completion.biblio.url.as_deref(),
        Some("https://doi.org/10.7717/peerj.4375")
    );
    assert_eq!(completion.biblio.authors.len(), 2);
    assert_eq!(
        completion.biblio.authors[0].surname.as_deref(),
        Some("Piwowar")
    );
    assert_eq!(
        completion.biblio.authors[0].orcid.as_deref(),
        Some("https://orcid.org/0000-0003-1613-5981")
    );

    let targets = requests.lock().expect("lock");
    assert_eq!(targets.len(), 1);
    assert!(
        targets[0].starts_with("/works/doi:10.7717"),
        "target: {}",
        targets[0]
    );
    assert!(targets[0].ends_with("peerj.4375"), "target: {}", targets[0]);
}

#[tokio::test]
async fn test_complete_by_title_when_doi_is_unknown() {
    let (addr, requests) = spawn_mock(|target| {
        if target.starts_with("/works/doi:") {
            MockResponse::status(404, r#"{"error": "not found", "message": "nope"}"#)
        } else {
            MockResponse::ok(search_response(WORK))
        }
    })
    .await;
    let biblio = Biblio {
        doi: Some("10.9999/unknown".to_string()),
        title: Some(
            "The state of OA: a large-scale analysis of the prevalence and impact of Open Access articles"
                .to_string(),
        ),
        ..Biblio::default()
    };

    let completion = completer(addr)
        .complete(&biblio)
        .await
        .expect("request succeeds")
        .expect("title match");

    assert_eq!(completion.matched_by, MatchKind::Title);
    assert_eq!(completion.biblio.journal.as_deref(), Some("PeerJ"));

    let targets = requests.lock().expect("lock");
    assert_eq!(targets.len(), 2);
    assert!(
        targets[0].contains("/works/doi:10.9999"),
        "target: {}",
        targets[0]
    );
    assert!(
        targets[0].contains("/works/doi:10.9999"),
        "target: {}",
        targets[0]
    );
    assert!(
        targets[1].contains("filter=title.search"),
        "target: {}",
        targets[1]
    );
    assert!(targets[1].contains("per-page=10"), "target: {}", targets[1]);
    assert!(
        targets[1].contains("sort=cited_by_count"),
        "target: {}",
        targets[1]
    );
}

#[tokio::test]
async fn test_complete_without_match_returns_none() {
    let empty = r#"{"meta": {"count": 0, "db_response_time_ms": 5, "page": 1, "per_page": 5, "groups_count": null}, "results": []}"#;
    let (addr, requests) = spawn_mock(move |_| MockResponse::ok(empty)).await;
    let biblio = Biblio {
        title: Some("A title that OpenAlex does not know".to_string()),
        ..Biblio::default()
    };

    let completion = completer(addr).complete(&biblio).await.expect("request");
    assert!(completion.is_none());
    assert_eq!(requests.lock().expect("lock").len(), 1);
}

#[tokio::test]
async fn test_no_lookup_without_doi_or_title() {
    let (addr, requests) = spawn_mock(|_| MockResponse::ok(WORK)).await;
    let completion = completer(addr)
        .complete(&Biblio::default())
        .await
        .expect("no request is made");
    assert!(completion.is_none());
    assert!(requests.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn test_http_error_is_reported() {
    let (addr, _) = spawn_mock(|_| MockResponse::status(500, "boom")).await;
    let biblio = Biblio {
        doi: Some("10.7717/peerj.4375".to_string()),
        ..Biblio::default()
    };

    match completer(addr).complete(&biblio).await {
        Err(Error::HttpStatus {
            status, message, ..
        }) => {
            assert_eq!(status, 500);
            assert_eq!(message, "boom");
        }
        other => panic!("expected an HTTP status error, got {other:?}"),
    }
}

#[tokio::test]
async fn test_rate_limit_is_reported() {
    let (addr, _) = spawn_mock(|_| MockResponse::status(429, "slow down")).await;
    let biblio = Biblio {
        doi: Some("10.7717/peerj.4375".to_string()),
        ..Biblio::default()
    };

    match completer(addr).complete(&biblio).await {
        Err(Error::HttpStatus {
            status, message, ..
        }) => {
            assert_eq!(status, 429);
            assert_eq!(message, "slow down");
        }
        other => panic!("expected an HTTP status error, got {other:?}"),
    }
}

#[tokio::test]
async fn test_search_error_is_reported() {
    let (addr, _) = spawn_mock(|target| {
        if target.starts_with("/works/doi:") {
            MockResponse::status(404, r#"{"error": "not found"}"#)
        } else {
            MockResponse::status(503, "unavailable")
        }
    })
    .await;
    let biblio = Biblio {
        doi: Some("10.9999/unknown".to_string()),
        title: Some("A title to search for".to_string()),
        ..Biblio::default()
    };

    match completer(addr).complete(&biblio).await {
        Err(Error::HttpStatus { status, .. }) => assert_eq!(status, 503),
        other => panic!("expected an HTTP status error, got {other:?}"),
    }
}

#[tokio::test]
async fn test_unparseable_response_is_reported() {
    let (addr, _) = spawn_mock(|_| MockResponse::ok("not json")).await;
    let biblio = Biblio {
        doi: Some("10.7717/peerj.4375".to_string()),
        ..Biblio::default()
    };

    match completer(addr).complete(&biblio).await {
        Err(Error::Parse { .. }) => {}
        other => panic!("expected a parse error, got {other:?}"),
    }
}

#[tokio::test]
async fn test_minimal_response_is_enough() {
    let (addr, _) = spawn_mock(|_| MockResponse::ok(r#"{"id": "https://openalex.org/W1"}"#)).await;
    let biblio = Biblio {
        doi: Some("10.1000/x".to_string()),
        title: Some("Some parsed title".to_string()),
        ..Biblio::default()
    };

    let completion = completer(addr)
        .complete(&biblio)
        .await
        .expect("tolerant parse")
        .expect("match");
    assert_eq!(completion.matched_by, MatchKind::Doi);
    assert_eq!(completion.openalex_id, "https://openalex.org/W1");
    // Nothing to fill: the parsed data is kept.
    assert_eq!(
        completion.biblio.title.as_deref(),
        Some("Some parsed title")
    );
    assert!(completion.biblio.authors.is_empty());
}

#[tokio::test]
async fn test_search_chooses_the_compatible_candidate() {
    let (addr, _) = spawn_mock(|target| {
        if target.starts_with("/works/doi:") {
            MockResponse::status(404, r#"{"error": "not found"}"#)
        } else {
            let results = format!("{WRONG_CANDIDATE}, {WORK}");
            MockResponse::ok(search_response(&results))
        }
    })
    .await;
    let biblio = Biblio {
        doi: Some("10.9999/unknown".to_string()),
        title: Some(
            "The state of OA: a large-scale analysis of the prevalence and impact of Open Access articles"
                .to_string(),
        ),
        date: Some("2018".to_string()),
        authors: vec![Author {
            surname: Some("Piwowar".to_string()),
            ..Author::default()
        }],
        ..Biblio::default()
    };

    let completion = completer(addr)
        .complete(&biblio)
        .await
        .expect("request")
        .expect("match");

    // The wrong-year, wrong-author candidate is skipped; the journal proves
    // that [`WORK`] was chosen.
    assert_eq!(completion.biblio.journal.as_deref(), Some("PeerJ"));
    assert_eq!(completion.biblio.volume.as_deref(), Some("6"));
}

#[tokio::test]
async fn test_doi_is_percent_encoded_in_the_path() {
    let (addr, requests) = spawn_mock(|_| MockResponse::ok(WORK)).await;
    let biblio = Biblio {
        doi: Some("10.1000/ABC/def".to_string()),
        ..Biblio::default()
    };

    completer(addr).complete(&biblio).await.expect("request");
    let targets = requests.lock().expect("lock");
    assert_eq!(targets.len(), 1);
    assert!(
        targets[0].contains("doi:10.1000%2FABC%2Fdef"),
        "target: {}",
        targets[0]
    );
}

#[tokio::test]
async fn test_title_with_comma_is_sanitized_for_the_filter() {
    let (addr, requests) = spawn_mock(|_| MockResponse::ok(search_response(COMMA_WORK))).await;
    let biblio = Biblio {
        title: Some("Sexual selection, sensory systems and sensory exploitation".to_string()),
        date: Some("1990".to_string()),
        authors: vec![Author {
            surname: Some("Ryan".to_string()),
            ..Author::default()
        }],
        ..Biblio::default()
    };

    let completion = completer(addr)
        .complete(&biblio)
        .await
        .expect("request")
        .expect("match");
    assert_eq!(completion.matched_by, MatchKind::Title);

    let targets = requests.lock().expect("lock");
    assert_eq!(targets.len(), 1);
    // The comma is gone from the filter value, so the API edge accepts it.
    assert!(
        targets[0].contains(
            "filter=title.search%3ASexual+selection+sensory+systems+and+sensory+exploitation"
        ),
        "target: {}",
        targets[0]
    );
    assert!(!targets[0].contains(','), "target: {}", targets[0]);
    assert!(!targets[0].contains("%2C"), "target: {}", targets[0]);
}

#[tokio::test]
async fn test_title_with_wildcard_is_sanitized() {
    let (addr, requests) = spawn_mock(|_| MockResponse::ok(search_response(WILDCARD_WORK))).await;
    let biblio = Biblio {
        title: Some(
            "Newborns' preferential tracking of face-like stimuli and its subsequent decline*"
                .to_string(),
        ),
        date: Some("1991".to_string()),
        authors: vec![Author {
            surname: Some("Johnson".to_string()),
            ..Author::default()
        }],
        ..Biblio::default()
    };

    let completion = completer(addr)
        .complete(&biblio)
        .await
        .expect("request")
        .expect("match");
    assert_eq!(completion.matched_by, MatchKind::Title);

    let targets = requests.lock().expect("lock");
    assert_eq!(targets.len(), 1);
    // The wildcard is gone; the apostrophe is percent-encoded as usual.
    assert!(
        targets[0].contains("filter=title.search%3ANewborns%27+preferential+tracking"),
        "target: {}",
        targets[0]
    );
    assert!(
        targets[0].contains("subsequent+decline"),
        "target: {}",
        targets[0]
    );
    assert!(!targets[0].contains('*'), "target: {}", targets[0]);
    assert!(!targets[0].contains("%3F"), "target: {}", targets[0]);
}

#[tokio::test]
async fn test_title_of_only_filter_characters_is_not_searched() {
    let (addr, requests) = spawn_mock(|_| MockResponse::ok(search_response(WORK))).await;
    let biblio = Biblio {
        title: Some(",,,,,,,,,,,,,,,,".to_string()),
        ..Biblio::default()
    };

    let completion = completer(addr).complete(&biblio).await.expect("no request");
    assert!(completion.is_none());
    assert!(requests.lock().expect("lock").is_empty());
}
