//! `pdf2bibtex`: process all PDFs in a directory with GROBID and write the
//! extracted bibliographic metadata as a BibTeX file.
//!
//! # Usage
//!
//! ```sh
//! cargo run --release --example pdf2bibtex -- ~/papers -s http://localhost:8070
//! ```
//!
//! Run with `--help` for all options. PDFs are discovered recursively; the
//! documents are processed through GROBID's `/api/processHeaderDocument`
//! service with a bounded number of concurrent requests, and one BibTeX
//! entry is written per document. Documents that fail to process are
//! reported on stderr and skipped.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use grobid::{Author, Biblio, Error, GrobidClient, PdfInput, ProcessOptions, RetryPolicy};
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
Usage: pdf2bibtex <DIR> [OPTIONS]

Reads all PDFs in <DIR> (recursively), extracts their bibliographic
metadata with GROBID and writes a BibTeX file.

Arguments:
  <DIR>                 directory containing the PDFs

Options:
  -o, --output <FILE>   output .bib file [default: <DIR>.bib, next to <DIR>]
  -s, --server <URL>    GROBID server URL [default: http://localhost:8070]
  -w, --workers <N>     number of concurrent requests [default: 4]
  -h, --help            print this help";

struct Args {
    input_dir: PathBuf,
    output: PathBuf,
    server_url: String,
    workers: usize,
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
        "processing {} PDF(s) from {} with {} worker(s)",
        pdfs.len(),
        args.input_dir.display(),
        args.workers
    );

    let options = ProcessOptions::default();
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

    let mut results: Vec<(PathBuf, Biblio)> = Vec::new();
    let mut failures = 0usize;
    while let Some(joined) = tasks.join_next().await {
        match joined.expect("worker task panicked") {
            Ok((path, biblio)) => {
                println!("ok:   {}", path.display());
                results.push((path, biblio));
            }
            Err(message) => {
                failures += 1;
                eprintln!("skip: {message}");
            }
        }
    }

    // Assign deterministic, collision-free keys and format the entries.
    let entries = format_entries(&results);
    if entries.is_empty() && failures > 0 {
        return Err(format!(
            "all {failures} document(s) failed to process; check the server and the messages above"
        )
        .into());
    }
    let bibtex = entries
        .iter()
        .map(|(_, entry)| entry.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    std::fs::write(&args.output, format!("{bibtex}\n"))?;
    println!(
        "wrote {} entr{} to {} ({} failed)",
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

/// Process a single PDF through GROBID's header service.
async fn process_pdf(
    client: &GrobidClient,
    path: &Path,
    options: &ProcessOptions,
) -> Result<(PathBuf, Biblio), String> {
    let document = client
        .process_header_document(PdfInput::from(path), options)
        .await
        .map_err(|err| format!("{}: {err}", path.display()))?;
    Ok((path.to_path_buf(), document.header.biblio))
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

/// Assign unique BibTeX keys and format all entries, sorted by key.
fn format_entries(results: &[(PathBuf, Biblio)]) -> Vec<(PathBuf, String)> {
    let mut keyed: Vec<(&Biblio, &PathBuf, String)> = results
        .iter()
        .map(|(path, biblio)| (biblio, path, bibtex_key(biblio)))
        .collect();
    keyed.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.1.cmp(b.1)));

    let mut previous: Option<String> = None;
    let mut suffix = 0usize;
    let mut entries = Vec::with_capacity(keyed.len());
    for (biblio, path, base) in keyed {
        if previous.as_deref() == Some(base.as_str()) {
            suffix += 1;
        } else {
            suffix = 0;
        }
        let key = if suffix == 0 {
            base.clone()
        } else {
            format!("{base}-{}", suffix + 1)
        };
        previous = Some(key.clone());
        entries.push((path.clone(), format_entry(&key, biblio)));
    }
    entries
}

/// Derive a BibTeX key from the first author's surname and the publication
/// year, e.g. `Kahle2000`. Falls back to `ref` (made unique by the suffix
/// logic in [`format_entries`]) when neither is known.
fn bibtex_key(biblio: &Biblio) -> String {
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

/// Pick a BibTeX entry type based on the available metadata.
fn entry_type(biblio: &Biblio) -> &'static str {
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
fn year(biblio: &Biblio) -> Option<String> {
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

/// Format one entry in BibTeX syntax.
fn format_entry(key: &str, biblio: &Biblio) -> String {
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

/// Add a BibTeX field line when the value is present and non-empty.
fn field(lines: &mut Vec<String>, name: &str, value: &Option<String>) {
    if let Some(value) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        lines.push(format!("  {name} = {{{value}}},"));
    }
}

/// Format authors or editors as `Surname, Given and ...`.
fn format_names(names: &[Author]) -> Option<String> {
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

fn parse_args() -> Result<Option<Args>, String> {
    let mut input_dir: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut server_url = grobid::DEFAULT_GROBID_URL.to_string();
    let mut workers = 4usize;
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < raw.len() {
        let arg = raw[i].clone();
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(None);
            }
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
            .join(format!("{name}.bib"))
    });
    Ok(Some(Args {
        input_dir,
        output,
        server_url,
        workers,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_wait_for_server_unreachable() {
        // Bind and release a port so that nothing is listening on it:
        // connection attempts are refused.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local addr");
        drop(listener);
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(1))
            .build()
            .expect("http client");
        let client = GrobidClient::builder(format!("http://{addr}"))
            .expect("client")
            .http_client(http)
            .retry(RetryPolicy {
                max_attempts: 1,
                ..RetryPolicy::default()
            })
            .build();
        let error = wait_for_server(&client, 2, Duration::from_millis(1))
            .await
            .expect_err("unreachable server must yield an error");
        let message = error.to_string();
        assert!(message.contains("cannot reach GROBID server"), "{message}");
        assert!(message.contains("after 2 attempts"), "{message}");
    }

    #[test]
    fn test_bibtex_key() {
        let mut biblio = Biblio::default();
        biblio.authors.push(Author {
            surname: Some("Milašauskienė".to_string()),
            ..Author::default()
        });
        biblio.date = Some("2003".to_string());
        assert_eq!(bibtex_key(&biblio), "Milaauskien2003");

        // Without author and date the key falls back to "ref"; uniqueness
        // is handled by the suffix logic in format_entries.
        assert_eq!(bibtex_key(&Biblio::default()), "ref");
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
        biblio.authors.push(Author {
            given_name: Some("Brewster".to_string()),
            surname: Some("Kahle".to_string()),
            ..Author::default()
        });
        biblio.authors.push(Author {
            given_name: Some("J".to_string()),
            surname: Some("Doe".to_string()),
            ..Author::default()
        });
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
    fn test_format_entries_unique_keys() {
        let make = |surname: &str| {
            let mut biblio = Biblio::default();
            biblio.authors.push(Author {
                surname: Some(surname.to_string()),
                ..Author::default()
            });
            biblio.date = Some("2020".to_string());
            biblio
        };
        // Three records, two with the same surname: keys must be unique and
        // deterministically suffixed.
        let results = vec![
            (PathBuf::from("b.pdf"), make("Smith")),
            (PathBuf::from("a.pdf"), make("Smith")),
            (PathBuf::from("c.pdf"), make("Jones")),
        ];
        let entries = format_entries(&results);
        let keys: Vec<&str> = entries
            .iter()
            .map(|(_, entry)| entry.lines().next().unwrap())
            .collect();
        assert_eq!(
            keys,
            vec!["@misc{Jones2020,", "@misc{Smith2020,", "@misc{Smith2020-2,"]
        );
    }
}
