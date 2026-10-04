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
//! in a `file` field.
//!
//! With `-a`/`--append` or `-m`/`--merge`, the entries go into an existing
//! BibTeX file instead of a new one. Both parse the target first, so that
//! generated citation keys stay unique. `--append` adds every entry;
//! `--merge` skips entries that are already in the file, where an entry
//! counts as present when its normalized field content, one of its
//! identifiers (DOI, PMID, arXiv) or its PDF file name matches an existing
//! record. Documents that fail to process are reported on stderr and
//! skipped. When built with the `openalex` feature, `--openalex` adds a
//! second completion tier against OpenAlex: headers are completed before
//! entries are formatted and PDFs are renamed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use biblatex::ChunksExt;
use grobid::bibtex;
use grobid::{Biblio, GrobidClient, PdfInput, ProcessOptions, RetryPolicy};
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
  -a, --append <FILE>   append all entries to an existing .bib file; the
                        file is parsed first to keep the new keys unique
  -m, --merge <FILE>    like --append, but skip entries whose content,
                        identifier (DOI, PMID, arXiv) or PDF file name
                        already occurs in the file
  -s, --server <URL>    GROBID server URL [default: http://localhost:8070]
  -w, --workers <N>     number of concurrent requests [default: 4]
  -r, --rename          rename each PDF to Auth_Year_<first 10 title words>
  -l, --link            add the PDF path as a file field to each entry
      --openalex        complete headers against OpenAlex; requires
                        building with --features openalex
  -h, --help            print this help

Only one of --output, --append and --merge may be given.";

struct Args {
    input_dir: PathBuf,
    destination: Destination,
    server_url: String,
    workers: usize,
    rename: bool,
    link: bool,
    openalex: bool,
}

/// Where the formatted entries are written: a new (or overwritten) output
/// file, or an existing BibTeX file that they are added to.
enum Destination {
    /// `-o`/`--output`: write all entries to this file.
    Output(PathBuf),
    /// `-a`/`--append`: append all entries to this existing BibTeX file. Its
    /// keys are parsed before processing, so that no generated key collides
    /// with an entry already in the file.
    Append(PathBuf),
    /// `-m`/`--merge`: append only the entries that are not already in this
    /// existing BibTeX file, judged by normalized field content, identifier
    /// (DOI, PMID, arXiv) or PDF file name.
    Merge(PathBuf),
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
    // With `--append` and `--merge`, parse the target file first: no PDF
    // must be processed (or renamed) when its keys cannot be determined,
    // and the keys seed the collision-free key assignment below. `--merge`
    // additionally builds the duplicate index from the existing records.
    let (reserved, mut seen) = match &args.destination {
        Destination::Append(path) => {
            let (keys, _) = load_existing(path)?;
            println!(
                "appending to {} ({})",
                path.display(),
                entry_count(keys.len())
            );
            (keys, MergeIndex::default())
        }
        Destination::Merge(path) => {
            let (keys, index) = load_existing(path)?;
            println!(
                "merging into {} ({})",
                path.display(),
                entry_count(keys.len())
            );
            (keys, index)
        }
        Destination::Output(_) => (HashSet::new(), MergeIndex::default()),
    };

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
    println!("waiting for GROBID at {} ...", probe.base_url());
    probe
        .wait_until_ready(PROBE_ATTEMPTS, PROBE_RETRY_DELAY)
        .await?;

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
    // With `--merge`, records already present in the target file are left
    // out.
    let (entries, duplicates) = match &args.destination {
        Destination::Merge(_) => merge_entries(&results, args.link, &mut seen, &reserved)?,
        _ => (format_entries(&results, args.link, &reserved), 0),
    };
    let bibtex = entries
        .iter()
        .map(|(_, entry)| entry.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let count = entry_count(entries.len());
    match &args.destination {
        Destination::Output(path) => {
            std::fs::write(path, format!("{bibtex}\n"))?;
            println!("wrote {count} to {} ({} failed)", path.display(), failures);
        }
        Destination::Append(path) => {
            append_entries(path, &bibtex)?;
            println!(
                "appended {count} to {} ({} failed)",
                path.display(),
                failures
            );
        }
        Destination::Merge(path) => {
            append_entries(path, &bibtex)?;
            println!(
                "merged {count} into {} ({} duplicate(s) skipped, {} failed)",
                path.display(),
                duplicates,
                failures
            );
        }
    }
    Ok(())
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

/// Formatted BibTeX entries together with the path of the PDF they were
/// extracted from.
type FormattedEntries = Vec<(PathBuf, String)>;

/// Assign unique BibTeX keys and format all entries, sorted by key. Keys in
/// `reserved` (e.g. parsed from the file given to `--append` or `--merge`)
/// are never reused: on a collision the key is suffixed with `-2`, `-3`,
/// ... With `link`, each entry records its PDF's path (as it is after
/// renaming) in a `file` field.
fn format_entries(
    results: &[(PathBuf, Biblio)],
    link: bool,
    reserved: &HashSet<String>,
) -> FormattedEntries {
    let mut used = reserved.clone();
    sorted_entries(results)
        .into_iter()
        .map(|(biblio, path, _)| {
            let key = bibtex::unique_key(biblio, &mut used);
            (path.clone(), render_entry(&key, biblio, path, link))
        })
        .collect()
}

/// Assign unique keys and format only the records that are not duplicates
/// of an entry in `seen` (the target file's records plus the ones accepted
/// from this batch). Returns the new entries and the number of records
/// skipped as duplicates, in the same order as [`format_entries`].
fn merge_entries(
    results: &[(PathBuf, Biblio)],
    link: bool,
    seen: &mut MergeIndex,
    reserved: &HashSet<String>,
) -> Result<(FormattedEntries, usize), Box<dyn std::error::Error>> {
    let mut used = reserved.clone();
    let mut entries = Vec::new();
    let mut duplicates = 0usize;
    for (biblio, path, suggested) in sorted_entries(results) {
        // Identify the record as it would be written; the key is not part
        // of the identity, so the suggested key can be used provisionally.
        let rendered = render_entry(&suggested, biblio, path, link);
        let identity = new_entry_identity(biblio, &rendered, path)?;
        if seen.contains(&identity) {
            duplicates += 1;
            continue;
        }
        let key = bibtex::unique_key(biblio, &mut used);
        entries.push((path.clone(), render_entry(&key, biblio, path, link)));
        seen.insert(identity);
    }
    Ok((entries, duplicates))
}

/// Records sorted by suggested key (and path, for determinism) before keys
/// are assigned collision-free.
fn sorted_entries(results: &[(PathBuf, Biblio)]) -> Vec<(&Biblio, &PathBuf, String)> {
    let mut keyed: Vec<(&Biblio, &PathBuf, String)> = results
        .iter()
        .map(|(path, biblio)| (biblio, path, bibtex::suggest_key(biblio)))
        .collect();
    keyed.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.1.cmp(b.1)));
    keyed
}

/// Render one record under `key`, adding the PDF path as `file` with
/// `link`.
fn render_entry(key: &str, biblio: &Biblio, path: &Path, link: bool) -> String {
    if link {
        bibtex::format_entry_with_file(key, biblio, path)
    } else {
        bibtex::format_entry(key, biblio)
    }
}

/// Parse the existing BibTeX file at `path` and return its citation keys
/// (for collision-free key assignment) and its duplicate index (for
/// `--merge`).
fn load_existing(path: &Path) -> Result<(HashSet<String>, MergeIndex), Box<dyn std::error::Error>> {
    let source = std::fs::read_to_string(path)
        .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    let bibliography = biblatex::Bibliography::parse(&source)
        .map_err(|err| format!("cannot parse {}: {err}", path.display()))?;
    let keys = bibliography.keys().map(str::to_string).collect();
    let index = bibliography.iter().map(entry_identity).collect();
    Ok((keys, index))
}

/// `1 entry`, `2 entries`, ...
fn entry_count(count: usize) -> String {
    format!("{count} entr{}", if count == 1 { "y" } else { "ies" })
}

/// The identity of a bibliography entry for `--merge`.
struct EntryIdentity {
    /// Normalized content of all fields except the citation key.
    fingerprint: String,
    /// Normalized identifiers, e.g. `doi:10.1234/x` or `pmid:123`.
    identifiers: Vec<String>,
    /// Lowercased PDF file name, from the `file` field or the input path.
    file_name: Option<String>,
}

/// The identities of all entries a `--merge` batch compares against: the
/// records of the target file plus the ones accepted so far.
#[derive(Default)]
struct MergeIndex {
    fingerprints: HashSet<String>,
    identifiers: HashSet<String>,
    file_names: HashSet<String>,
}

impl MergeIndex {
    /// Whether `identity` matches any known entry.
    fn contains(&self, identity: &EntryIdentity) -> bool {
        self.fingerprints.contains(&identity.fingerprint)
            || identity
                .identifiers
                .iter()
                .any(|identifier| self.identifiers.contains(identifier))
            || identity
                .file_name
                .as_ref()
                .is_some_and(|name| self.file_names.contains(name))
    }

    /// Record the identity of an accepted entry.
    fn insert(&mut self, identity: EntryIdentity) {
        self.fingerprints.insert(identity.fingerprint);
        self.identifiers.extend(identity.identifiers);
        self.file_names.extend(identity.file_name);
    }
}

impl FromIterator<EntryIdentity> for MergeIndex {
    fn from_iter<T: IntoIterator<Item = EntryIdentity>>(iter: T) -> Self {
        let mut index = Self::default();
        for identity in iter {
            index.insert(identity);
        }
        index
    }
}

/// The identity of an entry parsed from an existing file.
fn entry_identity(entry: &biblatex::Entry) -> EntryIdentity {
    EntryIdentity {
        fingerprint: entry_fingerprint(entry),
        identifiers: entry_identifiers(entry),
        file_name: entry_text(entry, "file")
            .or_else(|| entry_text(entry, "pdf"))
            .and_then(|file| normalize_file_name(&file)),
    }
}

/// The identity of a record about to be written: the rendered entry plus
/// the identifiers and the file name the renderer does not carry.
fn new_entry_identity(
    biblio: &Biblio,
    rendered: &str,
    path: &Path,
) -> Result<EntryIdentity, Box<dyn std::error::Error>> {
    let bibliography = biblatex::Bibliography::parse(rendered)
        .map_err(|err| format!("cannot parse rendered entry: {err}"))?;
    let entry = bibliography
        .iter()
        .next()
        .ok_or_else(|| format!("rendered entry is empty: {rendered}"))?;
    let mut identity = entry_identity(entry);
    identity.identifiers.extend(biblio_identifiers(biblio));
    identity.file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase());
    Ok(identity)
}

/// Normalized content of all fields of an entry, for exact-duplicate
/// detection. The citation key is not content; field names are already
/// lowercase, values are lowercased and whitespace-collapsed.
fn entry_fingerprint(entry: &biblatex::Entry) -> String {
    let mut fingerprint = format!("{:?}\n", entry.entry_type);
    for (name, value) in &entry.fields {
        fingerprint.push_str(name);
        fingerprint.push('=');
        fingerprint.push_str(&normalize_text(&value.format_verbatim()));
        fingerprint.push('\n');
    }
    fingerprint
}

/// Lowercase and collapse whitespace for content comparison.
fn normalize_text(raw: &str) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Normalized identifiers of a parsed entry, as `kind:value` strings. The
/// fields commonly used for them are all read: `doi`, `pmid`, `pmcid`,
/// `arxiv` and an `eprint` typed as arXiv.
fn entry_identifiers(entry: &biblatex::Entry) -> Vec<String> {
    let mut identifiers = Vec::new();
    if let Some(doi) = entry_text(entry, "doi").and_then(|raw| normalize_doi(&raw)) {
        identifiers.push(format!("doi:{doi}"));
    }
    if let Some(pmid) = entry_text(entry, "pmid").and_then(|raw| normalize_pmid(&raw)) {
        identifiers.push(format!("pmid:{pmid}"));
    }
    if let Some(pmcid) = entry_text(entry, "pmcid").and_then(|raw| normalize_pmcid(&raw)) {
        identifiers.push(format!("pmcid:{pmcid}"));
    }
    if let Some(arxiv) = entry_arxiv(entry) {
        identifiers.push(format!("arxiv:{arxiv}"));
    }
    identifiers
}

/// Normalized identifiers of a parsed record. The kinds mirror
/// [`entry_identifiers`], so that records written later match parsed ones.
fn biblio_identifiers(biblio: &Biblio) -> Vec<String> {
    let mut identifiers = Vec::new();
    if let Some(doi) = biblio.doi.as_deref().and_then(normalize_doi) {
        identifiers.push(format!("doi:{doi}"));
    }
    if let Some(pmid) = biblio.pmid.as_deref().and_then(normalize_pmid) {
        identifiers.push(format!("pmid:{pmid}"));
    }
    if let Some(pmcid) = biblio.pmcid.as_deref().and_then(normalize_pmcid) {
        identifiers.push(format!("pmcid:{pmcid}"));
    }
    if let Some(arxiv) = biblio.arxiv_id.as_deref().and_then(normalize_arxiv) {
        identifiers.push(format!("arxiv:{arxiv}"));
    }
    identifiers
}

/// The verbatim text of a field of a parsed entry.
fn entry_text(entry: &biblatex::Entry, field: &str) -> Option<String> {
    let text = entry.get(field)?.format_verbatim();
    let trimmed = text.trim();
    (!trimmed.is_empty()).then_some(trimmed.to_string())
}

/// The arXiv identifier of an entry: an `arxiv` field, or an `eprint`
/// field whose `eprinttype`/`archiveprefix` is arXiv.
fn entry_arxiv(entry: &biblatex::Entry) -> Option<String> {
    if let Some(arxiv) = entry_text(entry, "arxiv").and_then(|raw| normalize_arxiv(&raw)) {
        return Some(arxiv);
    }
    let kind = entry_text(entry, "eprinttype").or_else(|| entry_text(entry, "archiveprefix"))?;
    if !kind.eq_ignore_ascii_case("arxiv") {
        return None;
    }
    entry_text(entry, "eprint").and_then(|raw| normalize_arxiv(&raw))
}

/// Normalize a DOI: strip a resolver or `doi:` prefix, lowercase and trim.
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
    .find_map(|prefix| lower.strip_prefix(prefix))
    .unwrap_or(&lower)
    .trim_end_matches(['.', ',', ';'])
    .trim();
    (!stripped.is_empty()).then_some(stripped.to_string())
}

/// Normalize a PubMed ID: digits only, behind an optional `pmid:` prefix.
fn normalize_pmid(raw: &str) -> Option<String> {
    let lower = raw.trim().to_ascii_lowercase();
    let digits = lower.strip_prefix("pmid:").unwrap_or(&lower).trim();
    (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())).then_some(digits.to_string())
}

/// Normalize a PubMed Central ID: digits only, behind an optional
/// `pmcid:`/`pmc:` prefix and an optional `PMC`.
fn normalize_pmcid(raw: &str) -> Option<String> {
    let lower = raw.trim().to_ascii_lowercase();
    let rest = lower
        .strip_prefix("pmcid:")
        .or_else(|| lower.strip_prefix("pmc:"))
        .unwrap_or(&lower);
    let digits = rest.strip_prefix("pmc").unwrap_or(rest).trim();
    (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())).then_some(digits.to_string())
}

/// Normalize an arXiv identifier: strip an `arxiv:` prefix and a version
/// suffix, so `2404.14498v2` matches `2404.14498`.
fn normalize_arxiv(raw: &str) -> Option<String> {
    let lower = raw.trim().to_ascii_lowercase();
    let rest = lower.strip_prefix("arxiv:").unwrap_or(&lower).trim();
    let base = match rest.rsplit_once('v') {
        Some((base, version))
            if !base.is_empty()
                && !version.is_empty()
                && version.chars().all(|c| c.is_ascii_digit()) =>
        {
            base
        }
        _ => rest,
    };
    (!base.is_empty()).then_some(base.to_string())
}

/// The lowercased file-name component of a path or of a recorded `file`
/// field value.
fn normalize_file_name(raw: &str) -> Option<String> {
    let name = raw.rsplit(['/', '\\']).next()?.trim();
    (!name.is_empty()).then_some(name.to_lowercase())
}

/// Append `bibtex` to the existing file at `path`, keeping one blank line
/// between the existing entries and the appended ones.
fn append_entries(path: &Path, bibtex: &str) -> std::io::Result<()> {
    use std::io::Write;

    let existing = std::fs::read_to_string(path)?;
    let mut addition = String::new();
    if !existing.is_empty() {
        if !existing.ends_with('\n') {
            addition.push('\n');
        }
        if !existing.ends_with("\n\n") {
            addition.push('\n');
        }
    }
    addition.push_str(bibtex);
    addition.push('\n');
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)?
        .write_all(addition.as_bytes())
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
        let Some(target) = bibtex::suggest_file_name(&path, &results[index].1) else {
            eprintln!(
                "warn: cannot rename {}: no author, year or title extracted",
                path.display()
            );
            continue;
        };
        if target == path {
            continue; // already named as requested
        }
        let target = bibtex::unique_path(target);
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
    let mut append: Option<PathBuf> = None;
    let mut merge: Option<PathBuf> = None;
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
            "-o" | "--output" | "-a" | "--append" | "-m" | "--merge" | "-s" | "--server" | "-w"
            | "--workers" => {
                i += 1;
                let Some(value) = raw.get(i) else {
                    return Err(format!("missing value for {arg}"));
                };
                match arg.as_str() {
                    "-o" | "--output" => output = Some(PathBuf::from(value)),
                    "-a" | "--append" => append = Some(PathBuf::from(value)),
                    "-m" | "--merge" => merge = Some(PathBuf::from(value)),
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
    let destination = match (output, append, merge) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) | (_, Some(_), Some(_)) => {
            return Err("--output, --append and --merge are mutually exclusive".to_string());
        }
        (_, Some(path), None) => Destination::Append(path),
        (_, None, Some(path)) => Destination::Merge(path),
        (output, None, None) => Destination::Output(output.unwrap_or_else(|| {
            let name = input_dir
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "output".to_string());
            input_dir
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(format!("{name}.bib"))
        })),
    };
    Ok(Some(Args {
        input_dir,
        destination,
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
    async fn test_wait_until_ready_unreachable() {
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
        let error = client
            .wait_until_ready(2, Duration::from_millis(1))
            .await
            .expect_err("unreachable server must yield an error");
        let message = error.to_string();
        assert!(
            message.contains("did not respond in 2 attempt(s)"),
            "{message}"
        );
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
        let entries = format_entries(&results, false, &HashSet::new());
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
        let linked = format_entries(&results, true, &HashSet::new());
        assert!(
            linked[0].1.contains("file = {papers/a.pdf},"),
            "{}",
            linked[0].1
        );
        let plain = format_entries(&results, false, &HashSet::new());
        assert!(!plain[0].1.contains("file = "), "{}", plain[0].1);
    }

    #[test]
    fn test_format_entries_reserved_keys() {
        let biblio = Biblio {
            authors: vec![grobid::Author {
                surname: Some("Smith".to_string()),
                ..grobid::Author::default()
            }],
            date: Some("2020".to_string()),
            ..Biblio::default()
        };
        let results = vec![(PathBuf::from("a.pdf"), biblio)];
        // A key taken by an existing entry must not be reused: the new
        // entry is suffixed instead.
        let reserved: HashSet<String> = ["Smith2020".to_string()].into_iter().collect();
        let entries = format_entries(&results, false, &reserved);
        assert!(
            entries[0].1.starts_with("@misc{Smith2020-2,"),
            "{}",
            entries[0].1
        );
        // Without the reservation the natural key is used.
        let entries = format_entries(&results, false, &HashSet::new());
        assert!(
            entries[0].1.starts_with("@misc{Smith2020,"),
            "{}",
            entries[0].1
        );
    }

    /// A record for merge and append tests.
    fn merge_biblio(surname: &str, year: &str, doi: Option<&str>) -> Biblio {
        Biblio {
            authors: vec![grobid::Author {
                surname: Some(surname.to_string()),
                ..grobid::Author::default()
            }],
            date: Some(year.to_string()),
            title: Some("A title".to_string()),
            doi: doi.map(str::to_string),
            ..Biblio::default()
        }
    }

    #[test]
    fn test_append_into_existing_file() {
        let dir = std::env::temp_dir().join(format!("grobid-append-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("refs.bib");
        // The existing entry ends with a single newline; the appended entry
        // must be separated by one blank line.
        std::fs::write(&path, "@misc{Smith2019,\n  title = {Existing},\n}\n")
            .expect("write existing bib");

        let (reserved, _) = load_existing(&path).expect("load existing");
        assert_eq!(reserved, HashSet::from(["Smith2019".to_string()]));

        let biblio = merge_biblio("Smith", "2019", None);
        let results = vec![(PathBuf::from("a.pdf"), biblio)];
        let entries = format_entries(&results, false, &reserved);
        let bibtex = entries
            .iter()
            .map(|(_, entry)| entry.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        append_entries(&path, &bibtex).expect("append entries");

        let appended = std::fs::read_to_string(&path).expect("read appended bib");
        assert!(appended.starts_with("@misc{Smith2019,"), "{appended}");
        assert!(appended.contains("}\n\n@misc{Smith2019-2,"), "{appended}");
        assert!(appended.ends_with("}\n"), "{appended}");
        // The file itself parses back without duplicate keys.
        let bibliography = biblatex::Bibliography::parse(&appended).expect("parse appended bib");
        assert_eq!(bibliography.len(), 2);
        std::fs::remove_dir_all(&dir).expect("remove temp dir");
    }

    #[test]
    fn test_merge_skips_exact_duplicate() {
        let dir = std::env::temp_dir().join(format!("grobid-merge-exact-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("refs.bib");
        let biblio = merge_biblio("Smith", "2020", None);
        // A different key must not defeat the content comparison.
        let rendered = bibtex::format_entry("OldKey2020", &biblio);
        std::fs::write(&path, format!("{rendered}\n")).expect("write existing bib");

        let (reserved, mut index) = load_existing(&path).expect("load existing");
        let results = vec![(PathBuf::from("again.pdf"), biblio)];
        let (entries, duplicates) =
            merge_entries(&results, false, &mut index, &reserved).expect("merge");
        assert!(entries.is_empty(), "{entries:?}");
        assert_eq!(duplicates, 1);
        std::fs::remove_dir_all(&dir).expect("remove temp dir");
    }

    #[test]
    fn test_merge_skips_identifier_match() {
        let dir = std::env::temp_dir().join(format!("grobid-merge-doi-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("refs.bib");
        let existing = merge_biblio("Smith", "2020", Some("https://doi.org/10.1234/ABC.1"));
        let rendered = bibtex::format_entry("Smith2020", &existing);
        std::fs::write(&path, format!("{rendered}\n")).expect("write existing bib");

        let (reserved, mut index) = load_existing(&path).expect("load existing");
        // A different extraction of the same work: the DOI matches although
        // title and key differ.
        let mut fresh = merge_biblio("Smith", "2020", Some("doi:10.1234/abc.1"));
        fresh.title = Some("Another title".to_string());
        let results = vec![(PathBuf::from("b.pdf"), fresh)];
        let (entries, duplicates) =
            merge_entries(&results, false, &mut index, &reserved).expect("merge");
        assert!(entries.is_empty(), "{entries:?}");
        assert_eq!(duplicates, 1);
        std::fs::remove_dir_all(&dir).expect("remove temp dir");
    }

    #[test]
    fn test_merge_skips_same_file_name() {
        let dir = std::env::temp_dir().join(format!("grobid-merge-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("refs.bib");
        let existing = merge_biblio("Smith", "2019", Some("10.1/old"));
        let rendered =
            bibtex::format_entry_with_file("Smith2019", &existing, "papers/deep/paper.pdf");
        std::fs::write(&path, format!("{rendered}\n")).expect("write existing bib");

        let (reserved, mut index) = load_existing(&path).expect("load existing");
        // Different metadata and no identifier, but the same PDF file name.
        let mut fresh = merge_biblio("Jones", "2021", None);
        fresh.title = Some("Different".to_string());
        let results = vec![(PathBuf::from("/elsewhere/paper.pdf"), fresh)];
        let (entries, duplicates) =
            merge_entries(&results, true, &mut index, &reserved).expect("merge");
        assert!(entries.is_empty(), "{entries:?}");
        assert_eq!(duplicates, 1);
        std::fs::remove_dir_all(&dir).expect("remove temp dir");
    }

    #[test]
    fn test_merge_adds_new_and_dedupes_batch() {
        let dir = std::env::temp_dir().join(format!("grobid-merge-batch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("refs.bib");
        let existing = merge_biblio("Smith", "2019", Some("10.1/old"));
        let rendered = bibtex::format_entry("Smith2019", &existing);
        std::fs::write(&path, format!("{rendered}\n")).expect("write existing bib");

        let (reserved, mut index) = load_existing(&path).expect("load existing");
        let results = vec![
            // Already in the file.
            (PathBuf::from("a.pdf"), existing),
            // New, and once more in the same batch under a different DOI
            // spelling.
            (
                PathBuf::from("b.pdf"),
                merge_biblio("Jones", "2021", Some("10.1/new")),
            ),
            (
                PathBuf::from("c.pdf"),
                merge_biblio("Jones", "2021", Some("https://doi.org/10.1/NEW")),
            ),
        ];
        let (entries, duplicates) =
            merge_entries(&results, false, &mut index, &reserved).expect("merge");
        assert_eq!(duplicates, 2);
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].1.starts_with("@misc{Jones2021,"),
            "{}",
            entries[0].1
        );
        assert!(
            entries[0].1.contains("doi = {10.1/new}"),
            "{}",
            entries[0].1
        );
        std::fs::remove_dir_all(&dir).expect("remove temp dir");
    }

    #[test]
    fn test_normalize_identifiers() {
        assert_eq!(
            normalize_doi("https://doi.org/10.1234/ABC.1."),
            Some("10.1234/abc.1".to_string())
        );
        assert_eq!(normalize_pmid("PMID: 12345"), Some("12345".to_string()));
        assert_eq!(normalize_pmcid("PMC12345"), Some("12345".to_string()));
        assert_eq!(
            normalize_arxiv("arXiv:2404.14498v2"),
            Some("2404.14498".to_string())
        );
        assert_eq!(
            normalize_file_name("papers/deep/Paper.PDF"),
            Some("paper.pdf".to_string())
        );
    }

    #[test]
    fn test_append_entries_to_empty_file() {
        let dir = std::env::temp_dir().join(format!("grobid-merge-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("refs.bib");
        std::fs::write(&path, "").expect("write empty bib");
        assert!(load_existing(&path).expect("load").0.is_empty());
        append_entries(&path, "@misc{a,\n}").expect("append to empty file");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "@misc{a,\n}\n"
        );
        std::fs::remove_dir_all(&dir).expect("remove temp dir");
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
            bibtex::suggest_file_name(Path::new("dir/paper.pdf"), &biblio),
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
            bibtex::suggest_file_name(Path::new("papers/paper.PDF"), &biblio),
            Some(PathBuf::from("papers/2020_A_real_title.PDF"))
        );
        // Without any usable metadata the file keeps its name.
        assert_eq!(
            bibtex::suggest_file_name(Path::new("papers/paper.pdf"), &Biblio::default()),
            None
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
