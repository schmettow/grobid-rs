//! `refs2bibtex`: extract all bibliographic references from the PDFs in a
//! directory with GROBID and write them as a BibTeX file.
//!
//! # Usage
//!
//! ```sh
//! cargo run --release --example refs2bibtex -- ~/papers -s http://localhost:8070
//! ```
//!
//! Unlike the `pdf2bibtex` example, which writes one entry per document
//! (its header), this example writes one entry per reference found in the
//! documents (via `/api/processReferences`). References with an empty parse
//! result are skipped, references with the same DOI are deduplicated across
//! documents, and citation keys are made unique. Run with `--help` for all
//! options.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use grobid::bibtex;
use grobid::{
    Citation, CitationConsolidation, Error, GrobidClient, PdfInput, ProcessOptions, RetryPolicy,
};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Timeout for the server liveness probe. An unresponsive server must be
/// detected quickly, not after the long document processing timeout.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Timeout for a single document processing request.
const PROCESS_TIMEOUT: Duration = Duration::from_secs(600);
/// How often the liveness probe is attempted before giving up. GROBID can
/// take a while to become ready, e.g. while preloading its models.
const PROBE_ATTEMPTS: usize = 5;
/// Wait between liveness probe attempts.
const PROBE_RETRY_DELAY: Duration = Duration::from_secs(3);

const USAGE: &str = "\
Usage: refs2bibtex <DIR> [OPTIONS]

Reads all PDFs in <DIR> (recursively), extracts their bibliographic
references with GROBID and writes a BibTeX file with one entry per
reference.

Arguments:
  <DIR>                 directory containing the PDFs

Options:
  -o, --output <FILE>   output .bib file [default: <DIR>.refs.bib]
  -s, --server <URL>    GROBID server URL [default: http://localhost:8070]
  -w, --workers <N>     number of concurrent requests [default: 4]
  -c, --consolidate     consolidate references against CrossRef
  -h, --help            print this help";

struct Args {
    input_dir: PathBuf,
    output: PathBuf,
    server_url: String,
    workers: usize,
    consolidate: bool,
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(Some(args)) => args,
        Ok(None) => return ExitCode::SUCCESS, // --help
        Err(message) => {
            eprintln!("error: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let pdfs = collect_pdfs(&args.input_dir)?;
    if pdfs.is_empty() {
        return Err(format!("no PDFs found in {}", args.input_dir.display()).into());
    }

    // Probe the server before doing any work. The probe uses a short
    // timeout and bounded retries, so a server that does not respond is
    // reported quickly and clearly instead of hanging the batch.
    let probe_client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(PROBE_TIMEOUT)
        .build()?;
    let probe = GrobidClient::builder(&args.server_url)?
        .http_client(probe_client)
        .retry(RetryPolicy {
            max_attempts: 1,
            ..RetryPolicy::default()
        })
        .build();
    wait_for_server(&probe, PROBE_ATTEMPTS, PROBE_RETRY_DELAY).await?;

    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(PROCESS_TIMEOUT)
        .build()?;
    let client = GrobidClient::builder(&args.server_url)?
        .http_client(http)
        .build();

    println!(
        "extracting references from {} PDF(s) in {} with {} worker(s){}",
        pdfs.len(),
        args.input_dir.display(),
        args.workers,
        if args.consolidate {
            " (consolidated)"
        } else {
            ""
        }
    );

    let options = ProcessOptions {
        consolidate_citations: if args.consolidate {
            CitationConsolidation::Metadata
        } else {
            CitationConsolidation::None
        },
        ..ProcessOptions::default()
    };
    let semaphore = Arc::new(Semaphore::new(args.workers));
    let mut tasks = JoinSet::new();
    for pdf in &pdfs {
        let pdf = pdf.clone();
        let client = client.clone();
        let options = options.clone();
        let semaphore = Arc::clone(&semaphore);
        tasks.spawn(async move {
            // Bound the number of in-flight requests.
            let _permit = semaphore
                .acquire_owned()
                .await
                .expect("semaphore not closed");
            process_pdf(&client, &pdf, &options).await
        });
    }

    let mut results: Vec<(PathBuf, Vec<Citation>)> = Vec::new();
    let mut failures = 0usize;
    while let Some(joined) = tasks.join_next().await {
        match joined.expect("worker task panicked") {
            Ok((path, citations)) => {
                println!(
                    "ok:   {} ({} reference(s))",
                    path.display(),
                    citations.len()
                );
                results.push((path, citations));
            }
            Err(message) => {
                failures += 1;
                eprintln!("skip: {message}");
            }
        }
    }

    // Flatten the per-document lists, drop empty and duplicate references
    // and assign collision-free keys.
    let collected = collect_entries(results);
    println!(
        "{} reference(s) collected ({} skipped as empty, {} duplicate(s) dropped)",
        collected.citations.len(),
        collected.empty,
        collected.duplicates
    );
    if collected.citations.is_empty() {
        return Err(format!(
            "no references extracted from {} PDF(s) ({} failed)",
            pdfs.len(),
            failures
        )
        .into());
    }

    let entries = format_all(&collected.citations);
    let bibtex = entries.join("\n\n");
    std::fs::write(&args.output, format!("{bibtex}\n"))?;
    println!(
        "wrote {} entr{} to {} ({} document(s) failed)",
        entries.len(),
        if entries.len() == 1 { "y" } else { "ies" },
        args.output.display(),
        failures
    );
    Ok(())
}

/// Wait until the GROBID server responds and reports itself alive.
///
/// GROBID can take a while to become ready (e.g. while preloading its
/// models), so the probe is retried for a bounded time. A server that
/// refuses connections or does not answer within the probe timeout is
/// reported with a clear error instead of hanging the batch; a server
/// that responds with an unexpected HTTP status on `/api/isalive` (e.g.
/// a wrong base URL) fails immediately, since retrying cannot help.
async fn wait_for_server(
    client: &GrobidClient,
    attempts: usize,
    retry_delay: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    for attempt in 1..=attempts {
        let remaining = attempts - attempt;
        match client.ping().await {
            Ok(true) => return Ok(()),
            Ok(false) if remaining > 0 => {
                eprintln!(
                    "server at {} not alive yet; retrying in {}s ({}/{})",
                    client.base_url(),
                    retry_delay.as_secs(),
                    attempt,
                    attempts
                );
            }
            Ok(false) => {
                return Err(format!(
                    "GROBID server at {} is up, but reports it is not alive",
                    client.base_url()
                )
                .into());
            }
            Err(Error::HttpStatus { status, .. }) => {
                return Err(format!(
                    "GROBID server at {} answered HTTP {status} on /api/isalive; \
                     is the server URL correct?",
                    client.base_url()
                )
                .into());
            }
            Err(err) if remaining > 0 => {
                eprintln!(
                    "server at {} not responding ({err}); retrying in {}s ({}/{})",
                    client.base_url(),
                    retry_delay.as_secs(),
                    attempt,
                    attempts
                );
            }
            Err(err) => {
                return Err(format!(
                    "cannot reach GROBID server at {} after {attempts} attempts: {err}",
                    client.base_url()
                )
                .into());
            }
        }
        tokio::time::sleep(retry_delay).await;
    }
    unreachable!("the loop returns on its final attempt")
}

/// Process a single PDF through GROBID's reference extraction service.
async fn process_pdf(
    client: &GrobidClient,
    path: &Path,
    options: &ProcessOptions,
) -> Result<(PathBuf, Vec<Citation>), String> {
    let citations = client
        .process_references(PdfInput::from(path), options)
        .await
        .map_err(|err| format!("{}: {err}", path.display()))?;
    Ok((path.to_path_buf(), citations))
}

/// Recursively collect all PDFs under `dir`, in sorted order.
fn collect_pdfs(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                walk(&path, out)?;
            } else if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
            {
                out.push(path);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, &mut out)?;
    out.sort();
    Ok(out)
}

/// The flattened, deduplicated and deterministically ordered reference list.
struct Collected {
    citations: Vec<Citation>,
    /// References dropped because their parse result was empty.
    empty: usize,
    /// References dropped because their DOI was already seen.
    duplicates: usize,
}

/// Flatten the per-document reference lists into a single collection:
/// empty parse results are skipped, references sharing a DOI are kept only
/// once (the same paper cited by several documents yields one entry), and
/// the result is ordered by suggested citation key.
fn collect_entries(results: Vec<(PathBuf, Vec<Citation>)>) -> Collected {
    let mut seen_dois: HashSet<String> = HashSet::new();
    let mut keyed: Vec<(Citation, String, PathBuf, usize)> = Vec::new();
    let mut empty = 0usize;
    let mut duplicates = 0usize;
    for (path, citations) in results {
        for (index, citation) in citations.into_iter().enumerate() {
            if citation.is_empty() {
                empty += 1;
                continue;
            }
            if let Some(doi) = citation.doi.as_deref().map(str::to_ascii_lowercase) {
                if !seen_dois.insert(doi) {
                    duplicates += 1;
                    continue;
                }
            }
            let suggested = bibtex::suggest_key(&citation);
            keyed.push((citation, suggested, path.clone(), index));
        }
    }
    // Sort by suggested key; path and in-document index break ties for a
    // deterministic order across runs.
    keyed.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| a.3.cmp(&b.3))
    });
    Collected {
        citations: keyed.into_iter().map(|(citation, ..)| citation).collect(),
        empty,
        duplicates,
    }
}

/// Assign collision-free keys and format all entries.
fn format_all(citations: &[Citation]) -> Vec<String> {
    let mut used = HashSet::new();
    citations
        .iter()
        .map(|citation| bibtex::format_entry(&bibtex::unique_key(citation, &mut used), citation))
        .collect()
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut input_dir: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut server_url = grobid::DEFAULT_GROBID_URL.to_string();
    let mut workers = 4usize;
    let mut consolidate = false;
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < raw.len() {
        let arg = raw[i].clone();
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(None);
            }
            "-c" | "--consolidate" => consolidate = true,
            "-o" | "--output" | "-s" | "--server" | "-w" | "--workers" => {
                i += 1;
                let Some(value) = raw.get(i) else {
                    return Err(format!("missing value for {arg}"));
                };
                match arg.as_str() {
                    "-o" | "--output" => output = Some(PathBuf::from(value)),
                    "-s" | "--server" => server_url = value.clone(),
                    "-w" | "--workers" => {
                        workers = value
                            .parse()
                            .map_err(|_| format!("invalid worker count: {value}"))?
                    }
                    _ => unreachable!(),
                }
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option: {other}"));
            }
            other => {
                if input_dir.is_some() {
                    return Err("only one input directory is allowed".to_string());
                }
                input_dir = Some(PathBuf::from(other));
            }
        }
        i += 1;
    }
    let input_dir = input_dir.ok_or("missing input directory")?;
    if !input_dir.is_dir() {
        return Err(format!("{} is not a directory", input_dir.display()));
    }
    if workers == 0 {
        return Err("worker count must be at least 1".to_string());
    }
    let output = output.unwrap_or_else(|| {
        let name = input_dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "output".to_string());
        input_dir
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("{name}.refs.bib"))
    });
    Ok(Some(Args {
        input_dir,
        output,
        server_url,
        workers,
        consolidate,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use grobid::Biblio;

    fn citation(doi: Option<&str>, surname: Option<&str>) -> Citation {
        let mut biblio = Biblio::default();
        if let Some(surname) = surname {
            biblio.authors.push(grobid::Author {
                surname: Some(surname.to_string()),
                ..grobid::Author::default()
            });
        }
        biblio.doi = doi.map(str::to_string);
        biblio.date = Some("2020".to_string());
        Citation { index: 0, biblio }
    }

    #[test]
    fn test_collect_entries_dedupe_and_sort() {
        let results = vec![
            (
                PathBuf::from("b.pdf"),
                vec![
                    citation(Some("10.1/x"), Some("Zed")),
                    citation(Some("10.1/y"), Some("Able")),
                ],
            ),
            (
                PathBuf::from("a.pdf"),
                vec![citation(Some("10.1/x"), Some("Zed")), Citation::default()],
            ),
        ];
        let collected = collect_entries(results);
        // The empty citation is skipped and the duplicate DOI dropped.
        assert_eq!(collected.empty, 1);
        assert_eq!(collected.duplicates, 1);
        let keys: Vec<String> = collected
            .citations
            .iter()
            .map(|citation| bibtex::suggest_key(citation))
            .collect();
        assert_eq!(keys, vec!["Able2020", "Zed2020"]);
    }

    #[test]
    fn test_format_all_unique_keys() {
        let citations = vec![
            citation(None, Some("Jones")),
            citation(None, Some("Smith")),
            citation(None, Some("Smith")),
        ];
        let entries = format_all(&citations);
        let keys: Vec<&str> = entries
            .iter()
            .map(|entry| entry.lines().next().unwrap())
            .collect();
        assert_eq!(
            keys,
            vec!["@misc{Jones2020,", "@misc{Smith2020,", "@misc{Smith2020-2,"]
        );
    }
}
