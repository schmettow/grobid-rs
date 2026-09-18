//! Tests for the GROBID API client, exercised against a minimal in-process
//! mock HTTP server. The mock records request details (method, path,
//! headers, body) so that the multipart and urlencoded encoding can be
//! verified without a running GROBID instance.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use grobid::{CoordinateElement, Error, GrobidClient, PdfInput, ProcessOptions, RetryPolicy};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A canned HTTP response of the mock server.
struct MockResponse {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl MockResponse {
    fn ok(body: &str) -> Self {
        Self {
            status: 200,
            content_type: "application/xml",
            body: body.as_bytes().to_vec(),
        }
    }

    fn text(status: u16, body: &str) -> Self {
        Self {
            status,
            content_type: "text/plain",
            body: body.as_bytes().to_vec(),
        }
    }
}

/// A parsed HTTP request as seen by the mock server.
struct MockRequest {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl MockRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn body_str(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Spawn a mock GROBID server. `handler` is invoked for every request and
/// returns either a response or an error description; the results are
/// collected so that tests can assert on them (assertions in the spawned
/// task would otherwise be silently swallowed).
async fn spawn_mock<F>(handler: F) -> (SocketAddr, Arc<Mutex<Vec<Result<(), String>>>>)
where
    F: Fn(&MockRequest) -> Result<MockResponse, String> + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let results = Arc::new(Mutex::new(Vec::new()));
    let task_results = Arc::clone(&results);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let outcome = serve_connection(&mut stream, &handler).await;
            task_results.lock().expect("lock").push(outcome);
        }
    });
    (addr, results)
}

/// Serve a single connection: read the request, invoke the handler, write
/// the response, and report the handler's verdict.
async fn serve_connection<F>(stream: &mut tokio::net::TcpStream, handler: &F) -> Result<(), String>
where
    F: Fn(&MockRequest) -> Result<MockResponse, String>,
{
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    let header_end = loop {
        let n = stream
            .read(&mut tmp)
            .await
            .map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            return Err("connection closed before headers".to_string());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
        .collect();
    let content_length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = stream
            .read(&mut tmp)
            .await
            .map_err(|e| format!("read body: {e}"))?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(content_length);
    let request = MockRequest {
        method,
        target,
        headers,
        body,
    };

    let response = handler(&request)?;
    let reason = match response.status {
        200 => "OK",
        204 => "No Content",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        reason,
        response.content_type,
        response.body.len()
    );
    stream
        .write_all(head.as_bytes())
        .await
        .map_err(|e| format!("write head: {e}"))?;
    stream
        .write_all(&response.body)
        .await
        .map_err(|e| format!("write body: {e}"))?;
    let _ = stream.shutdown().await;
    Ok(())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// A client with fast retries for the mock server.
fn test_client(addr: SocketAddr, max_attempts: usize) -> GrobidClient {
    GrobidClient::builder(format!("http://{addr}"))
        .expect("client")
        .retry(RetryPolicy {
            max_attempts,
            initial_delay: Duration::from_millis(5),
            max_delay: Duration::from_millis(20),
            multiplier: 2.0,
        })
        .build()
}

fn assert_all_ok(results: &Arc<Mutex<Vec<Result<(), String>>>>) {
    let results = results.lock().expect("lock");
    assert!(
        results.iter().all(|r| r.is_ok()),
        "mock server errors: {results:?}"
    );
}

#[tokio::test]
async fn test_fulltext_roundtrip() {
    let tei = std::fs::read_to_string("testdata/document/example.tei.xml").expect("fixture");
    let (addr, results) = spawn_mock(move |req| {
        if req.method != "POST" {
            return Err(format!("method: {}", req.method));
        }
        if req.target != "/api/processFulltextDocument" {
            return Err(format!("target: {}", req.target));
        }
        let content_type = req
            .header("content-type")
            .ok_or("missing content-type")?
            .to_string();
        if !content_type.starts_with("multipart/form-data; boundary=") {
            return Err(format!("content-type: {content_type}"));
        }
        let body = req.body_str();
        for expected in [
            "name=\"input\"",
            "filename=\"paper.pdf\"",
            "application/pdf",
            "consolidateHeader",
            "consolidateCitations",
            "generateIDs",
            "includeRawCitations",
            "teiCoordinates",
            "ref",
            "biblStruct",
            "segmentSentences",
        ] {
            if !body.contains(expected) {
                return Err(format!("body missing {expected:?}"));
            }
        }
        Ok(MockResponse::ok(&tei))
    })
    .await;

    let client = test_client(addr, 1);
    let options = ProcessOptions {
        generate_ids: true,
        include_raw_citations: true,
        tei_coordinates: CoordinateElement::COMMON.to_vec(),
        segment_sentences: true,
        ..Default::default()
    };
    let pdf = PdfInput::Data {
        filename: "paper.pdf".to_string(),
        data: b"%PDF-1.4 test".to_vec(),
    };
    let document = client
        .process_fulltext_document(pdf, &options)
        .await
        .expect("process fulltext");
    assert_all_ok(&results);
    assert_eq!(
        document.header.title.as_deref(),
        Some(
            "Changes of patients' satisfaction with the health care services in \
             Lithuanian Health Promoting Hospitals network"
        )
    );
    assert_eq!(document.citations.len(), 15);
}

#[tokio::test]
async fn test_retry_on_503() {
    let tei = std::fs::read_to_string("testdata/document/example.tei.xml").expect("fixture");
    let calls = Arc::new(AtomicU32::new(0));
    let handler_calls = Arc::clone(&calls);
    let (addr, results) = spawn_mock(move |req| {
        let call = handler_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if req.target != "/api/processFulltextDocument" {
            return Err(format!("target: {}", req.target));
        }
        if call < 3 {
            Ok(MockResponse::text(503, "busy"))
        } else {
            Ok(MockResponse::ok(&tei))
        }
    })
    .await;

    let client = test_client(addr, 3);
    let document = client
        .process_fulltext_document(
            PdfInput::Data {
                filename: "paper.pdf".to_string(),
                data: b"%PDF-1.4".to_vec(),
            },
            &Default::default(),
        )
        .await
        .expect("process after retries");
    assert_all_ok(&results);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(document.citations.len(), 15);
}

#[tokio::test]
async fn test_server_busy_exhausted() {
    let calls = Arc::new(AtomicU32::new(0));
    let handler_calls = Arc::clone(&calls);
    let (addr, results) = spawn_mock(move |_req| {
        handler_calls.fetch_add(1, Ordering::SeqCst);
        Ok(MockResponse::text(503, "busy"))
    })
    .await;

    let client = test_client(addr, 2);
    let error = client
        .process_fulltext_document(
            PdfInput::Data {
                filename: "paper.pdf".to_string(),
                data: b"%PDF-1.4".to_vec(),
            },
            &Default::default(),
        )
        .await
        .expect_err("should give up");
    assert_all_ok(&results);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(matches!(error, Error::ServerBusy { attempts: 2 }));
}

#[tokio::test]
async fn test_citation_list() {
    let tei = std::fs::read_to_string("testdata/citation_list/example.tei.xml").expect("fixture");
    let (addr, results) = spawn_mock(move |req| {
        if req.method != "POST" {
            return Err(format!("method: {}", req.method));
        }
        if req.target != "/api/processCitationList" {
            return Err(format!("target: {}", req.target));
        }
        let content_type = req
            .header("content-type")
            .ok_or("missing content-type")?
            .to_string();
        if !content_type.starts_with("application/x-www-form-urlencoded") {
            return Err(format!("content-type: {content_type}"));
        }
        let body = req.body_str();
        for expected in [
            "citations=Smith+2001",
            "citations=Jones+2002",
            "consolidateCitations=1",
            "includeRawCitations=1",
        ] {
            if !body.contains(expected) {
                return Err(format!("body missing {expected:?}"));
            }
        }
        Ok(MockResponse::ok(&tei))
    })
    .await;

    let client = test_client(addr, 1);
    let options = ProcessOptions {
        consolidate_citations: grobid::CitationConsolidation::Metadata,
        include_raw_citations: true,
        ..Default::default()
    };
    let citations = client
        .process_citation_list(["Smith 2001", "Jones 2002"], &options)
        .await
        .expect("process citation list");
    assert_all_ok(&results);
    assert_eq!(citations.len(), 13);
}

#[tokio::test]
async fn test_single_citation_bare_biblstruct() {
    // The response is a bare <biblStruct> without any namespace, as
    // GROBID's /api/processCitation returns.
    let bare = r#"<biblStruct >
    <analytic>
        <title level="a" type="main">Graff</title>
        <author>
            <persName xmlns="http://www.tei-c.org/ns/1.0"><surname>Graff</surname></persName>
        </author>
    </analytic>
    <monogr>
        <title level="j">Expert. Opin. Ther. Targets</title>
        <imprint>
            <biblScope unit="volume">6</biblScope>
            <biblScope unit="issue">1</biblScope>
            <biblScope unit="page" from="103" to="113" />
            <date type="published" when="2002" />
        </imprint>
    </monogr>
</biblStruct>"#;
    let (addr, results) = spawn_mock(move |req| {
        if req.target != "/api/processCitation" {
            return Err(format!("target: {}", req.target));
        }
        let body = req.body_str();
        if !body.contains("citations=Graff%2C+Expert") {
            return Err(format!("body: {body}"));
        }
        Ok(MockResponse::ok(bare))
    })
    .await;

    let client = test_client(addr, 1);
    let citation = client
        .process_citation(
            "Graff, Expert. Opin. Ther. Targets (2002)",
            &Default::default(),
        )
        .await
        .expect("process citation")
        .expect("non-empty citation");
    assert_all_ok(&results);
    assert_eq!(
        citation.journal.as_deref(),
        Some("Expert. Opin. Ther. Targets")
    );
    assert_eq!(citation.volume.as_deref(), Some("6"));
    assert_eq!(citation.issue.as_deref(), Some("1"));
    assert_eq!(citation.pages.as_deref(), Some("103-113"));
    assert_eq!(citation.date.as_deref(), Some("2002"));
    assert_eq!(citation.authors[0].surname.as_deref(), Some("Graff"));
}

#[tokio::test]
async fn test_citation_no_content() {
    let (addr, results) = spawn_mock(|_req| Ok(MockResponse::text(204, ""))).await;
    let client = test_client(addr, 1);
    let citation = client
        .process_citation("unparseable", &Default::default())
        .await
        .expect("process citation");
    assert_all_ok(&results);
    assert!(citation.is_none());
}

#[tokio::test]
async fn test_references_no_content() {
    let (addr, results) = spawn_mock(|_req| Ok(MockResponse::text(204, ""))).await;
    let client = test_client(addr, 1);
    let references = client
        .process_references(
            PdfInput::Data {
                filename: "paper.pdf".to_string(),
                data: b"%PDF-1.4".to_vec(),
            },
            &Default::default(),
        )
        .await
        .expect("process references");
    assert_all_ok(&results);
    assert!(references.is_empty());
}

#[tokio::test]
async fn test_fulltext_no_content_is_error() {
    let (addr, results) = spawn_mock(|_req| Ok(MockResponse::text(204, ""))).await;
    let client = test_client(addr, 1);
    let error = client
        .process_fulltext_document(
            PdfInput::Data {
                filename: "paper.pdf".to_string(),
                data: b"%PDF-1.4".to_vec(),
            },
            &Default::default(),
        )
        .await
        .expect_err("204 yields NoContent");
    assert_all_ok(&results);
    assert!(matches!(error, Error::NoContent));
}

#[tokio::test]
async fn test_http_error_status() {
    let (addr, results) =
        spawn_mock(|_req| Ok(MockResponse::text(500, "the sky is falling"))).await;
    let client = test_client(addr, 1);
    let error = client
        .process_fulltext_document(
            PdfInput::Data {
                filename: "paper.pdf".to_string(),
                data: b"%PDF-1.4".to_vec(),
            },
            &Default::default(),
        )
        .await
        .expect_err("500 yields an error");
    assert_all_ok(&results);
    match error {
        Error::HttpStatus {
            status, message, ..
        } => {
            assert_eq!(status, 500);
            assert_eq!(message, "the sky is falling");
        }
        other => panic!("expected HttpStatus, got {other:?}"),
    }
}

#[tokio::test]
async fn test_ping() {
    let (addr, results) = spawn_mock(move |req| {
        if req.method != "GET" {
            return Err(format!("method: {}", req.method));
        }
        match req.target.as_str() {
            "/api/isalive" => Ok(MockResponse::text(200, "true")),
            "/api/version" => Ok(MockResponse::text(200, "0.9.1")),
            other => Err(format!("target: {other}")),
        }
    })
    .await;

    let client = test_client(addr, 1);
    assert!(client.ping().await.expect("ping"));
    assert_eq!(client.version().await.expect("version"), "0.9.1");
    assert_all_ok(&results);
}

#[tokio::test]
async fn test_ping_false() {
    let (addr, results) = spawn_mock(move |req| {
        if req.target != "/api/isalive" {
            return Err(format!("target: {}", req.target));
        }
        Ok(MockResponse::text(200, "false"))
    })
    .await;

    let client = test_client(addr, 1);
    assert!(!client.ping().await.expect("ping"));
    assert_all_ok(&results);
}

#[test]
fn test_base_url_normalization() {
    let client = GrobidClient::new("http://localhost:8070").expect("client");
    assert_eq!(client.base_url().as_str(), "http://localhost:8070/");
    let client = GrobidClient::new("localhost:8070").expect("client");
    assert_eq!(client.base_url().as_str(), "http://localhost:8070/");
    let client = GrobidClient::new("https://grobid.example.org/grobid/").expect("client");
    assert_eq!(
        client.base_url().as_str(),
        "https://grobid.example.org/grobid/"
    );
    assert!(GrobidClient::new("http://[::1").is_err());
}
