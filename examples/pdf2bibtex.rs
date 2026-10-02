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
//! entry is written per document. With `-r`/`--rename`, each PDF is renamed
//! to `Auth_Year_<first 10 title words>.pdf` once its metadata has been
//! extracted; with `-l`/`--link`, every entry records the path of its PDF
//! in a `file` field. Documents that fail to process are reported on stderr
//! and skipped. When built with the `openalex` feature, `--openalex` adds a
//! second completion tier against OpenAlex: headers are completed before
//! entries are formatted and PDFs are renamed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use grobid::bibtex;
use grobid::{Biblio, Error, GrobidClient, PdfInput, ProcessOptions, RetryPolicy};
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
  -r, --rename          rename each PDF to Auth_Year_<first 10 title words>
  -l, --link            add the PDF path as a file field to each entry
      --openalex        complete headers against OpenAlex; requires
                        building with --features openalex
  -h, --help            print this help";

struct Args {
    input_dir: PathBuf,
    output: PathBuf,
    server_url: String,
    workers: usize,
    rename: bool,
    link: bool,
    openalex: bool,
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

    let mut notes = Vec::new();
    if args.rename {
        notes.push("renaming PDFs");
    }
    if args.openalex {
        notes.push("OpenAlex completion");
    }
    let notes = if notes.is_empty() {
        String::new()
    } else {
        format!(" ({})", notes.join(", "))
    };
    println!(
        "processing {} PDF(s) from {} with {} worker(s){notes}",
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

    if results.is_empty() {
        return Err(format!(
            "all {failures} document(s) failed to process; check the server and the messages above"
        )
        .into());
    }

    // Second tier: fill the gaps GROBID left behind from OpenAlex, before
    // renaming so that file names are built from the completed metadata.
    #[cfg(feature = "openalex")]
    if args.openalex {
        results = complete_headers(results, args.workers).await?;
    }

    // Rename before formatting, so that `--link` entries point at the
    // files' final locations. A failed rename keeps the original path, so
    // the entry still refers to an existing file.
    if args.rename {
        rename_pdfs(&mut results);
    }

    // Assign deterministic, collision-free keys and format the entries.
    let entries = format_entries(&results, args.link);
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

/// Assign unique BibTeX keys and format all entries, sorted by key. With
/// `link`, each entry records its PDF's path (as it is after renaming) in
/// a `file` field.
fn format_entries(results: &[(PathBuf, Biblio)], link: bool) -> Vec<(PathBuf, String)> {
    // Sort by suggested key (and path, for determinism) before assigning
    // collision-free keys.
    let mut keyed: Vec<(&Biblio, &PathBuf, String)> = results
        .iter()
        .map(|(path, biblio)| (biblio, path, bibtex::suggest_key(biblio)))
        .collect();
    keyed.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.1.cmp(b.1)));

    let mut used = HashSet::new();
    keyed
        .into_iter()
        .map(|(biblio, path, _)| {
            let key = bibtex::unique_key(biblio, &mut used);
            let entry = if link {
                bibtex::format_entry_with_file(&key, biblio, path)
            } else {
                bibtex::format_entry(&key, biblio)
            };
            (path.clone(), entry)
        })
        .collect()
}

/// Rename every processed PDF to its [`rename_target`] name, keeping the
/// original file extension, and update `results` to the files' final
/// locations (so that `--link` entries point at the renamed files). Files
/// are handled in path order, so collision suffixes (`-2`, `-3`, ...) are
/// deterministic; a PDF that already bears the requested name is left
/// alone. Individual rename errors are reported on stderr and do not stop
/// the batch.
fn rename_pdfs(results: &mut [(PathBuf, Biblio)]) {
    // Rename in path order so that collision suffixes are deterministic.
    let mut order: Vec<usize> = (0..results.len()).collect();
    order.sort_by(|&a, &b| results[a].0.cmp(&results[b].0));
    let mut renamed = 0usize;
    for index in order {
        let path = results[index].0.clone();
        let Some(target) = rename_target(&path, &results[index].1) else {
            eprintln!(
                "warn: cannot rename {}: no author, year or title extracted",
                path.display()
            );
            continue;
        };
        if target == path {
            continue; // already named as requested
        }
        let target = first_free(target);
        match std::fs::rename(&path, &target) {
            Ok(()) => {
                renamed += 1;
                println!("renamed: {} -> {}", path.display(), target.display());
                results[index].0 = target;
            }
            Err(err) => eprintln!(
                "warn: cannot rename {} to {}: {err}",
                path.display(),
                target.display()
            ),
        }
    }
    println!("renamed {renamed} of {} PDF(s)", results.len());
}

/// Build the new file name `Auth_Year_<first 10 title words>` for a
/// processed PDF, keeping the original file extension. The parts are
/// filtered to ASCII alphanumerics (non-ASCII letters are dropped, as in
/// [`bibtex::suggest_key`]); missing parts are omitted. Returns `None` when
/// neither author, year nor a title word is available.
fn rename_target(path: &Path, biblio: &Biblio) -> Option<PathBuf> {
    let author = biblio
        .authors
        .first()
        .and_then(|author| author.surname.as_deref())
        .map(sanitize)
        .unwrap_or_default();
    let year = bibtex::year(biblio).unwrap_or_default();
    let title = biblio
        .title
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
        .take(10)
        .map(sanitize)
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    let stem = [author, year, title]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    if stem.is_empty() {
        return None;
    }
    let mut target = path.with_file_name(stem);
    if let Some(extension) = path.extension() {
        target.set_extension(extension);
    }
    Some(target)
}

/// The first free variant of `target`, extended with a `-2`, `-3`, ...
/// suffix on collisions (like [`bibtex::unique_key`], but for files).
fn first_free(target: PathBuf) -> PathBuf {
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

/// Filter `text` down to ASCII alphanumerics, as [`bibtex::suggest_key`]
/// does for citation keys.
fn sanitize(text: &str) -> String {
    text.chars().filter(char::is_ascii_alphanumeric).collect()
}

/// Complete the extracted document headers against OpenAlex.
///
/// Each header is looked up independently, with the same worker bound as the
/// GROBID requests. Lookups that fail are reported and leave the header
/// unchanged, so an unreachable or rate-limited OpenAlex API does not lose
/// data. The order of `results` is kept.
#[cfg(feature = "openalex")]
async fn complete_headers(
    results: Vec<(PathBuf, Biblio)>,
    workers: usize,
) -> Result<Vec<(PathBuf, Biblio)>, Box<dyn std::error::Error>> {
    let total = results.len();
    let completer = grobid::openalex::Completer::new();
    let semaphore = Arc::new(Semaphore::new(workers));
    let mut tasks = JoinSet::new();
    for (index, (path, biblio)) in results.into_iter().enumerate() {
        let completer = completer.clone();
        let semaphore = Arc::clone(&semaphore);
        tasks.spawn(async move {
            // Bound the number of concurrent OpenAlex lookups.
            let _permit = semaphore
                .acquire_owned()
                .await
                .expect("semaphore not closed");
            let outcome = completer.complete(&biblio).await;
            (index, path, biblio, outcome)
        });
        // OpenAlex asks the common (keyless) pool for at most 10 requests
        // per second, so pace the dispatch of the workers.
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let mut completed = Vec::with_capacity(total);
    let mut matched = 0usize;
    let mut failed = 0usize;
    while let Some(joined) = tasks.join_next().await {
        let (index, path, mut biblio, outcome) = joined.expect("worker task panicked");
        match outcome {
            Ok(Some(completion)) => {
                matched += 1;
                biblio = completion.biblio;
            }
            Ok(None) => {}
            Err(err) => {
                failed += 1;
                eprintln!("openalex: {}: {err}", path.display());
            }
        }
        completed.push((index, path, biblio));
    }
    completed.sort_by_key(|(index, _, _)| *index);
    println!("openalex: matched {matched} of {total} document(s) ({failed} lookup(s) failed)");
    Ok(completed
        .into_iter()
        .map(|(_, path, biblio)| (path, biblio))
        .collect())
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut input_dir: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut server_url = grobid::DEFAULT_GROBID_URL.to_string();
    let mut workers = 4usize;
    let mut rename = false;
    let mut link = false;
    #[cfg(feature = "openalex")]
    let mut openalex = false;
    #[cfg(not(feature = "openalex"))]
    let openalex = false;
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < raw.len() {
        let arg = raw[i].clone();
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(None);
            }
            "-r" | "--rename" => rename = true,
            "-l" | "--link" => link = true,
            "--openalex" => {
                #[cfg(not(feature = "openalex"))]
                {
                    return Err("--openalex requires the `openalex` feature; rebuild with \
                                `cargo run --features openalex --example pdf2bibtex`"
                        .to_string());
                }
                #[cfg(feature = "openalex")]
                {
                    openalex = true;
                }
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
        rename,
        link,
        openalex,
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
    fn test_format_entries_unique_keys() {
        let make = |surname: &str| {
            let mut biblio = Biblio::default();
            biblio.authors.push(grobid::Author {
                surname: Some(surname.to_string()),
                ..grobid::Author::default()
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
        let entries = format_entries(&results, false);
        let keys: Vec<&str> = entries
            .iter()
            .map(|(_, entry)| entry.lines().next().unwrap())
            .collect();
        assert_eq!(
            keys,
            vec!["@misc{Jones2020,", "@misc{Smith2020,", "@misc{Smith2020-2,"]
        );
    }

    #[test]
    fn test_format_entries_link() {
        let biblio = Biblio {
            authors: vec![grobid::Author {
                surname: Some("Smith".to_string()),
                ..grobid::Author::default()
            }],
            date: Some("2020".to_string()),
            title: Some("A title".to_string()),
            ..Biblio::default()
        };
        let results = vec![(PathBuf::from("papers/a.pdf"), biblio)];
        let linked = format_entries(&results, true);
        assert!(
            linked[0].1.contains("file = {papers/a.pdf},"),
            "{}",
            linked[0].1
        );
        let plain = format_entries(&results, false);
        assert!(!plain[0].1.contains("file = "), "{}", plain[0].1);
    }

    #[test]
    fn test_rename_target() {
        let biblio = Biblio {
            authors: vec![grobid::Author {
                surname: Some("Milašauskienė".to_string()),
                ..grobid::Author::default()
            }],
            date: Some("2003".to_string()),
            title: Some(
                "One two three four five six seven eight nine ten eleven twelve".to_string(),
            ),
            ..Biblio::default()
        };
        // Non-ASCII letters are dropped (as in `bibtex::suggest_key`), only
        // the first ten title words are used, and the extension is kept.
        assert_eq!(
            rename_target(Path::new("dir/paper.pdf"), &biblio),
            Some(PathBuf::from(
                "dir/Milaauskien_2003_One_two_three_four_five_six_seven_eight_nine_ten.pdf"
            ))
        );
    }

    #[test]
    fn test_rename_target_missing_parts() {
        // Missing parts are omitted; punctuation-only title words are
        // dropped.
        let biblio = Biblio {
            date: Some("2020-01-30".to_string()),
            title: Some("--- A *real* title".to_string()),
            ..Biblio::default()
        };
        assert_eq!(
            rename_target(Path::new("papers/paper.PDF"), &biblio),
            Some(PathBuf::from("papers/2020_A_real_title.PDF"))
        );
        // Without any usable metadata the file keeps its name.
        assert_eq!(
            rename_target(Path::new("papers/paper.pdf"), &Biblio::default()),
            None
        );
    }

    #[test]
    fn test_with_suffix() {
        assert_eq!(
            with_suffix(Path::new("dir/Smith2020_Title.pdf"), 2),
            PathBuf::from("dir/Smith2020_Title-2.pdf")
        );
    }

    #[test]
    fn test_rename_pdfs_collision() {
        let dir = std::env::temp_dir().join(format!("grobid-rename-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let make = |name: &str| {
            let path = dir.join(name);
            std::fs::write(&path, b"pdf").expect("write file");
            path
        };
        let biblio = || Biblio {
            authors: vec![grobid::Author {
                surname: Some("Smith".to_string()),
                ..grobid::Author::default()
            }],
            date: Some("2020".to_string()),
            title: Some("Same title".to_string()),
            ..Biblio::default()
        };
        // Both records map to the same name; the first in path order gets
        // the plain name, the second a `-2` suffix.
        let mut results = vec![(make("b.pdf"), biblio()), (make("a.pdf"), biblio())];
        rename_pdfs(&mut results);
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .expect("read temp dir")
            .map(|entry| entry.expect("dir entry").file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["Smith_2020_Same_title-2.pdf", "Smith_2020_Same_title.pdf"]
        );
        // The results now point at the renamed files, for `--link` output.
        let mut paths: Vec<String> = results
            .iter()
            .map(|(path, _)| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        paths.sort();
        assert_eq!(paths, names);
        std::fs::remove_dir_all(&dir).expect("remove temp dir");
    }
}
