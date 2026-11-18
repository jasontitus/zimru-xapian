//! `zimdump` — CLI parity with kiwix `zim-tools`' `zimdump`.
//!
//! Subcommands (mirrors upstream):
//!
//! ```text
//! zimdump info    [--ns=N]                       <file>
//! zimdump list    [--details] [--idx=I|(--url=U [--ns=N])] <file>
//! zimdump show    (--idx=I|(--url=U [--ns=N]))   <file>
//! zimdump dump    --dir=DIR [--ns=N] [--redirect] <file>
//! zimdump analyze [--by-item]                    <file>   (zimru extension)
//! zimdump --help | --version
//! ```
//!
//! Exit codes match upstream: 0 = ok, 1 = no/multiple matches, 2 = dump error.
//!
//! `analyze` is a zimru-only subcommand that has no upstream counterpart
//! (see issue #5). It prints either a per-cluster table (default) or a
//! per-item table (`--by-item`) showing where storage is going inside the
//! archive: compressed cluster size, decompressed payload, and ratio.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use zimru::{Archive, Dirent, Entry, Error, NS_ARTICLES_LEGACY};

const VERSION: &str = "zimdump (zimru) 0.1.0\n+ libzim equivalent: zimru 0.1.0";

#[cfg(unix)]
fn restore_sigpipe() {
    // SAFETY: install the platform's default action before starting any workers.
    // A closed output pipe should terminate this CLI as usual for Unix tools.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}
#[cfg(not(unix))]
fn restore_sigpipe() {}

fn main() -> ExitCode {
    restore_sigpipe();
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(String::as_str);
    let res = match sub {
        Some("info") => cmd_info(&args[2..]),
        Some("list") => cmd_list(&args[2..]),
        Some("show") => cmd_show(&args[2..]),
        Some("dump") => cmd_dump(&args[2..]),
        Some("analyze") => cmd_analyze(&args[2..]),
        Some("-h") | Some("--help") | None => {
            print_usage();
            return ExitCode::SUCCESS;
        }
        Some("--version") => {
            println!("{VERSION}");
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("zimdump: unknown subcommand `{other}`");
            print_usage();
            return ExitCode::from(2);
        }
    };
    match res {
        Ok(code) => code,
        Err(e) => {
            eprintln!("zimdump: {e}");
            ExitCode::from(2)
        }
    }
}

fn print_usage() {
    println!(
        "\nzimdump tool is used to inspect a zim file and also to dump its contents into the filesystem.\n\nUsage:\n  zimdump list [--details] [--idx=INDEX|([--url=URL] [--ns=N])] [--] <file>\n  zimdump dump --dir=DIR [--ns=N] [--redirect] [--] <file>\n  zimdump show (--idx=INDEX|(--url=URL [--ns=N])) [--] <file>\n  zimdump info [--ns=N] [--] <file>\n  zimdump analyze [--by-item] [--] <file>   (zimru extension)\n  zimdump -h | --help\n  zimdump --version\n"
    );
}

#[derive(Default)]
struct Opts {
    file: Option<String>,
    idx: Option<u32>,
    url: Option<String>,
    ns: Option<u8>,
    details: bool,
    dir: Option<PathBuf>,
    redirect: bool,
    by_item: bool,
}

fn parse_opts(args: &[String]) -> Result<Opts, Error> {
    let mut o = Opts::default();
    let mut i = 0;
    let mut positional: Vec<String> = Vec::new();
    while i < args.len() {
        let a = &args[i];
        if let Some(v) = a.strip_prefix("--idx=") {
            o.idx = Some(v.parse().map_err(|_| io_err("bad --idx"))?);
        } else if a == "--idx" {
            i += 1;
            o.idx = Some(
                args.get(i)
                    .ok_or_else(|| io_err("--idx needs a value"))?
                    .parse()
                    .map_err(|_| io_err("bad --idx"))?,
            );
        } else if let Some(v) = a.strip_prefix("--url=") {
            o.url = Some(v.to_string());
        } else if a == "--url" {
            i += 1;
            o.url = Some(
                args.get(i)
                    .ok_or_else(|| io_err("--url needs a value"))?
                    .clone(),
            );
        } else if let Some(v) = a.strip_prefix("--ns=") {
            o.ns = Some(parse_ns(v)?);
        } else if a == "--ns" {
            i += 1;
            o.ns = Some(parse_ns(
                args.get(i).ok_or_else(|| io_err("--ns needs a value"))?,
            )?);
        } else if let Some(v) = a.strip_prefix("--dir=") {
            o.dir = Some(PathBuf::from(v));
        } else if a == "--dir" {
            i += 1;
            o.dir = Some(PathBuf::from(
                args.get(i).ok_or_else(|| io_err("--dir needs a value"))?,
            ));
        } else if a == "--details" {
            o.details = true;
        } else if a == "--redirect" {
            o.redirect = true;
        } else if a == "--by-item" {
            o.by_item = true;
        } else if a == "--" {
            // rest are positional
            i += 1;
            while i < args.len() {
                positional.push(args[i].clone());
                i += 1;
            }
            break;
        } else if a.starts_with("--") {
            return Err(io_err(&format!("unknown option `{a}`")));
        } else {
            positional.push(a.clone());
        }
        i += 1;
    }
    if positional.len() == 1 {
        o.file = Some(positional.into_iter().next().unwrap());
    } else if positional.len() > 1 {
        return Err(io_err("expected exactly one <file> positional argument"));
    }
    Ok(o)
}

fn parse_ns(s: &str) -> Result<u8, Error> {
    if s.len() == 1 {
        Ok(s.as_bytes()[0])
    } else {
        Err(io_err(&format!(
            "--ns expects a single character, got `{s}`"
        )))
    }
}

fn open_archive(opts: &Opts) -> Result<Archive, Error> {
    let path = opts.file.as_ref().ok_or_else(|| io_err("missing <file>"))?;
    Archive::open(path)
}

// ---------------- subcommand: info ----------------

fn cmd_info(args: &[String]) -> Result<ExitCode, Error> {
    let opts = parse_opts(args)?;
    let arc = open_archive(&opts)?;
    let h = arc.header();
    // count-entries: only C namespace in modern format (matches upstream behavior).
    let count = count_in_namespace(&arc, opts.ns.unwrap_or(b'C'))?;
    println!("count-entries: {count}");
    println!("uuid: {}", format_uuid(&h.uuid));
    println!("cluster count: {}", h.cluster_count);
    if h.has_checksum() {
        let cs = arc.checksum()?;
        println!("checksum: {}", hex_lower(&cs));
    }
    // Main page url (the actual content target, follows the redirect)
    if arc.has_main_entry() {
        let main = arc.main_entry()?;
        let resolved = follow_redirect(&main)?;
        println!("main page: {}", resolved.path());
    }
    // Favicon: check if M/Illustration_48x48@1 exists
    if arc.entry_by_ns_path(b'M', "Illustration_48x48@1").is_ok() {
        println!("favicon: Illustration_48x48@1");
    }
    // Upstream zimdump info ends with a trailing blank line.
    println!();
    Ok(ExitCode::SUCCESS)
}

fn count_in_namespace(arc: &Archive, ns: u8) -> Result<u64, Error> {
    Ok(arc.entry_count_in_namespace(ns)? as u64)
}

fn follow_redirect(e: &Entry) -> Result<Entry, Error> {
    let mut cur = e.clone();
    let mut depth = 0;
    while cur.is_redirect() {
        depth += 1;
        if depth > 16 {
            return Err(Error::RedirectLoop);
        }
        cur = cur.get_redirect_entry()?;
    }
    Ok(cur)
}

// ---------------- subcommand: list ----------------

fn cmd_list(args: &[String]) -> Result<ExitCode, Error> {
    let opts = parse_opts(args)?;
    let arc = open_archive(&opts)?;
    let target_ns = opts.ns.unwrap_or(b'C');

    if let Some(idx) = opts.idx {
        let e = entry_by_filtered_index(&arc, target_ns, idx)?;
        print_list_entry(&e, opts.details, idx);
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(url) = &opts.url {
        let e = arc.entry_by_ns_path(target_ns, url)?;
        let idx = filtered_index_of(&arc, target_ns, e.index())?;
        print_list_entry(&e, opts.details, idx);
        return Ok(ExitCode::SUCCESS);
    }

    // List entire namespace.
    let mut local_idx: u32 = 0;
    for entry in arc.iter_by_path() {
        let e = entry?;
        if e.namespace() != target_ns {
            continue;
        }
        print_list_entry(&e, opts.details, local_idx);
        local_idx += 1;
    }
    Ok(ExitCode::SUCCESS)
}

fn entry_by_filtered_index(arc: &Archive, ns: u8, idx: u32) -> Result<Entry, Error> {
    // A namespace occupies one contiguous run of the URL-pointer list,
    // so the idx-th entry of a namespace is just an offset into that
    // O(log n) range — no linear scan over every dirent.
    let range = arc.namespace_range(ns)?;
    let global = range
        .start
        .checked_add(idx)
        .filter(|&g| g < range.end)
        .ok_or(Error::EntryNotFound)?;
    arc.entry_by_url_index(global)
}

fn filtered_index_of(arc: &Archive, ns: u8, target_global: u32) -> Result<u32, Error> {
    let range = arc.namespace_range(ns)?;
    if !range.contains(&target_global) {
        return Err(Error::EntryNotFound);
    }
    Ok(target_global - range.start)
}

fn print_list_entry(e: &Entry, details: bool, local_idx: u32) {
    if !details {
        println!("{}", e.path());
        return;
    }
    println!("path: {}", e.path());
    println!("* title:          {}", e.title());
    println!("* idx:            {}", local_idx);
    if e.is_redirect() {
        println!("* type:           redirect");
        if let Ok(target) = e.get_redirect_entry() {
            // Upstream prints redirect index as the target's filtered C-index.
            // We approximate using the target's global url-index — for content
            // namespace this is identical when the target is also in C.
            println!("* redirect index: {}", target.index());
        }
    } else if let Ok(item) = e.get_item(false) {
        println!("* type:           item");
        println!("* mime-type:      {}", item.mimetype());
        if let Ok(sz) = item.size() {
            println!("* item size:      {sz}");
        }
    }
}

// ---------------- subcommand: show ----------------

/// Namespace to look `--url` up in. Upstream documents the default as `A`
/// (the legacy article namespace) and relies on libzim's compatibility layer
/// to redirect that to `C` on archives written with the new namespace scheme
/// — so `zimdump show --url=Foo` works on both generations. Resolving `A`
/// literally, as we used to, made every `--url` lookup fail on any modern
/// archive.
fn effective_url_ns(arc: &Archive, requested: Option<u8>) -> u8 {
    let ns = requested.unwrap_or(NS_ARTICLES_LEGACY);
    if ns == NS_ARTICLES_LEGACY && arc.header().uses_new_namespaces() {
        b'C'
    } else {
        ns
    }
}

/// Resolve a `--url` value to an entry. The plain path wins; a
/// `<namespace>/<path>` form (`M/Title`, `C/Foo`) is accepted as a fallback
/// so callers can address metadata and index entries the same way upstream's
/// users do. Trying the plain path first keeps a content path that genuinely
/// begins with a one-character directory (`C/foo.png`) resolving to itself.
fn lookup_by_url(arc: &Archive, requested_ns: Option<u8>, url: &str) -> Result<Entry, Error> {
    let ns = effective_url_ns(arc, requested_ns);
    match arc.entry_by_ns_path(ns, url) {
        Ok(e) => Ok(e),
        Err(e) => {
            if requested_ns.is_none() {
                let b = url.as_bytes();
                if b.len() > 2 && b[1] == b'/' && b[0].is_ascii_alphanumeric() {
                    if let Ok(hit) = arc.entry_by_ns_path(b[0], &url[2..]) {
                        return Ok(hit);
                    }
                }
            }
            Err(e)
        }
    }
}

fn cmd_show(args: &[String]) -> Result<ExitCode, Error> {
    let opts = parse_opts(args)?;
    let arc = open_archive(&opts)?;
    let entry = if let Some(idx) = opts.idx {
        entry_by_filtered_index(&arc, opts.ns.unwrap_or(b'C'), idx)?
    } else if let Some(url) = &opts.url {
        lookup_by_url(&arc, opts.ns, url)?
    } else {
        eprintln!("zimdump: --idx or --url required for `show`");
        return Ok(ExitCode::from(1));
    };
    if entry.is_redirect() {
        // Upstream message: "Entry <path> is a redirect." — on *stderr*,
        // with a failing exit status, since `show` produced no content.
        eprintln!("Entry {} is a redirect.", entry.path());
        return Ok(ExitCode::from(255));
    }
    let item = entry.get_item(false)?;
    io::stdout().write_all(item.get_data()?.data())?;
    Ok(ExitCode::SUCCESS)
}

// ---------------- subcommand: analyze ----------------

fn cmd_analyze(args: &[String]) -> Result<ExitCode, Error> {
    let opts = parse_opts(args)?;
    let arc = open_archive(&opts)?;
    let n_clusters = arc.cluster_count();

    // First pass: per-cluster compressed size (cheap — pointer arithmetic)
    // and decompressed total (needs decoding each cluster once). For
    // `--by-item`, per-blob sizes are retained here so the item loop
    // below never has to re-decode a cluster: URL order visits clusters
    // near-randomly, so answering `item.size()` from the cluster cache
    // instead would thrash the LRU and re-decompress the same clusters
    // up to once per blob.
    struct ClusterInfo {
        compressed: u64,
        decompressed: u64,
        blob_count: u32,
        compression: zimru::Compression,
        blob_sizes: Vec<u64>,
    }
    let mut clusters: Vec<ClusterInfo> = Vec::with_capacity(n_clusters as usize);
    for idx in 0..n_clusters {
        let range = arc.cluster_byte_range(idx)?;
        let compressed = range.end - range.start;
        let c = arc.cluster_uncached(idx)?;
        let blob_sizes: Vec<u64> = (0..c.blob_count())
            .map(|b| c.blob(b).map(|d| d.len() as u64).unwrap_or(0))
            .collect();
        let decompressed: u64 = blob_sizes.iter().sum();
        clusters.push(ClusterInfo {
            compressed,
            decompressed,
            blob_count: c.blob_count(),
            compression: c.compression(),
            blob_sizes: if opts.by_item { blob_sizes } else { Vec::new() },
        });
    }

    if opts.by_item {
        println!(
            "{:<6} {:<6} {:<10} {:<8} {:<14} {:<6} {:<14} path",
            "clstr", "blob", "compress", "blobs", "decomp(B)", "share", "est_comp(B)"
        );
        for entry in arc.iter_by_path() {
            let e = entry?;
            let (cluster_idx, blob_idx) = match e.dirent() {
                Dirent::Article(a) => (a.cluster, a.blob),
                Dirent::Redirect(_) => continue,
            };
            // `cluster_idx` comes from an untrusted dirent field — a
            // corrupt archive can point past the cluster table, so skip
            // (never index) out-of-range values.
            let Some(info) = clusters.get(cluster_idx as usize) else {
                continue;
            };
            let blob_size = info.blob_sizes.get(blob_idx as usize).copied().unwrap_or(0);
            let share = if info.decompressed == 0 {
                0.0
            } else {
                blob_size as f64 / info.decompressed as f64
            };
            let est_compressed = (share * info.compressed as f64).round() as u64;
            let compression = format!("{:?}", info.compression).to_lowercase();
            let ns = char::from(e.namespace());
            let path = e.path();
            println!(
                "{cluster_idx:<6} {blob_idx:<6} {compression:<10} {:<8} {blob_size:<14} {share:<6.3} {est_compressed:<14} {ns}/{path}",
                info.blob_count,
            );
        }
        return Ok(ExitCode::SUCCESS);
    }

    // Per-cluster summary table.
    println!(
        "{:<6} {:<10} {:<8} {:<14} {:<14} ratio",
        "clstr", "compress", "blobs", "compressed(B)", "decompressed(B)"
    );
    let mut total_compressed: u64 = 0;
    let mut total_decompressed: u64 = 0;
    for (idx, info) in clusters.iter().enumerate() {
        let ratio = if info.decompressed == 0 {
            0.0
        } else {
            info.compressed as f64 / info.decompressed as f64
        };
        println!(
            "{:<6} {:<10} {:<8} {:<14} {:<14} {:.3}",
            idx,
            format!("{:?}", info.compression).to_lowercase(),
            info.blob_count,
            info.compressed,
            info.decompressed,
            ratio,
        );
        total_compressed += info.compressed;
        total_decompressed += info.decompressed;
    }
    let total_ratio = if total_decompressed == 0 {
        0.0
    } else {
        total_compressed as f64 / total_decompressed as f64
    };
    println!(
        "{:<6} {:<10} {:<8} {:<14} {:<14} {:.3}",
        "TOTAL", "-", "-", total_compressed, total_decompressed, total_ratio,
    );
    Ok(ExitCode::SUCCESS)
}

// ---------------- subcommand: dump ----------------

fn cmd_dump(args: &[String]) -> Result<ExitCode, Error> {
    use parking_lot::Mutex;
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let opts = parse_opts(args)?;
    let arc = open_archive(&opts)?;
    let dir = opts
        .dir
        .clone()
        .ok_or_else(|| io_err("dump requires --dir=DIR"))?;
    // Like `list`/`info`, dump only the content namespace by default on
    // modern archives (matches upstream zimdump 3.8.0, verified black-box);
    // otherwise `M/Title` and `C/Title` would flatten onto one path.
    // Legacy archives spread content over `A`/`I`/`-`, so there the
    // default stays "every namespace" and `--ns` narrows it.
    let target_ns = opts
        .ns
        .or_else(|| arc.header().uses_new_namespaces().then_some(b'C'));

    // Filesystem path collisions are expected in real archives: an
    // entry `Foo` (a file) can coexist with `Foo/bar` (which needs
    // `Foo` to be a directory). Upstream zimdump resolves these by
    // writing the colliding entry to `DIR/_exceptions/` with the
    // path percent-escaped. Plan those destinations before publishing.
    let exceptions_dir = PathBuf::from("_exceptions");
    // Pass 1 — dirents only, no cluster decompression: partition into
    // articles keyed by (cluster, blob) and redirects with their
    // resolved target paths.
    let mut articles: Vec<(u32, u32, String)> = Vec::new();
    let mut redirects: Vec<(String, String)> = Vec::new();
    for entry in arc.iter_by_path() {
        let e = entry?;
        if let Some(ns) = target_ns {
            if e.namespace() != ns {
                continue;
            }
        }
        match e.dirent() {
            Dirent::Article(a) => articles.push((a.cluster, a.blob, safe_path(e.path()))),
            Dirent::Redirect(_) => {
                let target = e.get_redirect_entry()?;
                redirects.push((safe_path(e.path()), safe_path(target.path())));
            }
        }
    }

    let paths = articles
        .iter()
        .map(|(_, _, r)| r.as_str())
        .chain(redirects.iter().map(|(r, _)| r.as_str()));
    let mut plan = plan_dump(paths, &dir, &exceptions_dir)?;
    let exiled_redirects = plan.exiled.split_off(articles.len());
    let exiled_articles = plan.exiled;
    let root = match plan.root {
        Some(existing) => existing,
        None => DumpRoot::open(&dir, plan.temp_namespace, true)?,
    };
    let errors = AtomicUsize::new(0);
    let errlog: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let fallback_paths: Mutex<HashMap<String, String>> = Mutex::new(HashMap::new());
    let record_fallback = |rel: &str, exc: &std::path::Path| {
        fallback_paths
            .lock()
            .insert(rel.to_owned(), exc.to_string_lossy().into_owned());
        eprintln!(
            "Wrote {} to {}",
            dir.join(rel).display(),
            dir.join(exc).display()
        );
    };
    let log_failure = |dest: &std::path::Path, err: &io::Error| {
        errors.fetch_add(1, Ordering::Relaxed);
        errlog.lock().push(format!("{}: {}", dest.display(), err));
    };
    // Write an entry at its natural path, or — when `exiled` says its
    // path is occupied by a shallower entry — directly under
    // `_exceptions/`. A collision that slips through anyway (unusual
    // filesystems) still falls back to `_exceptions/` at runtime.
    // `attempt` performs the actual filesystem operation (file write
    // or symlink creation) against a root-relative path.
    let write_with_fallback =
        |rel: &str, exiled: bool, attempt: &dyn Fn(&PathBuf) -> io::Result<()>| {
            let dest = PathBuf::from(rel);
            if exiled {
                let exc = exception_dest(&exceptions_dir, rel);
                match attempt(&exc).map(|()| exc) {
                    Ok(exc) => record_fallback(rel, &exc),
                    Err(err) => log_failure(&dest, &err),
                }
                return;
            }
            match attempt(&dest) {
                Ok(()) => {}
                Err(err) if !plan.fixed_fallbacks && is_collision(&err) => {
                    let exc = exception_dest(&exceptions_dir, rel);
                    match attempt(&exc).map(|()| exc) {
                        Ok(exc) => record_fallback(rel, &exc),
                        Err(err2) => log_failure(&dest, &err2),
                    }
                }
                Err(err) => log_failure(&dest, &err),
            }
        };

    // Redirects can refer forward to another redirect. Record statically
    // known relocations before emitting any links, then record runtime
    // article fallbacks as they are written.
    for ((rel, _), &exiled) in redirects.iter().zip(&exiled_redirects) {
        if exiled {
            fallback_paths.lock().insert(
                rel.clone(),
                exception_dest(&exceptions_dir, rel)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }

    // Pass 2 — extract articles grouped by cluster so each cluster is
    // decompressed exactly once. URL order visits clusters in a near-
    // random sequence on real archives (the scraper packs clusters in
    // crawl order, not URL order), which used to thrash the LRU and
    // re-decompress the same clusters hundreds of times. Clusters are
    // processed in parallel; `cluster_uncached` bypasses the shared
    // cache so peak memory is one decompressed cluster per thread.
    let mut arts: Vec<(u32, u32, String, bool)> = articles
        .into_iter()
        .zip(exiled_articles)
        .map(|((c, b, r), e)| (c, b, r, e))
        .collect();
    arts.sort_unstable_by_key(|a| (a.0, a.1));
    // One group per source cluster: (blob_idx, rel_path, exiled).
    type ClusterGroup = (u32, Vec<(u32, String, bool)>);
    let mut groups: Vec<ClusterGroup> = Vec::new();
    for (cluster, blob, rel, exiled) in arts {
        match groups.last_mut() {
            Some((c, v)) if *c == cluster => v.push((blob, rel, exiled)),
            _ => groups.push((cluster, vec![(blob, rel, exiled)])),
        }
    }
    groups
        .par_iter()
        .try_for_each(|(cluster_idx, blobs)| -> Result<(), Error> {
            let cluster = arc.cluster_uncached(*cluster_idx)?;
            for (blob_idx, rel, exiled) in blobs {
                let data = cluster.blob(*blob_idx)?;
                write_with_fallback(rel, *exiled, &|dest| root.write(dest, data));
            }
            Ok(())
        })?;

    // Pass 3 — redirects retain their single-step semantics. Both symlinks
    // and HTML links are relative to the actual destination of the stub,
    // including the exception directory when a source path collides.
    for ((rel, target_rel), exiled) in redirects.iter().zip(exiled_redirects) {
        let relocated_target = fallback_paths.lock().get(target_rel).cloned();
        let target_rel = relocated_target.as_deref().unwrap_or(target_rel);
        write_with_fallback(rel, exiled, &|dest| {
            let target = relative_target(dest, target_rel);
            if opts.redirect {
                root.symlink(dest, &target)
            } else {
                let url = escape_html(&redirect_url(&target));
                let label = escape_html(&target);
                let html = format!(
                    "<html><head><meta http-equiv=\"refresh\" content=\"0; url={url}\"></head><body><a href=\"{url}\">{label}</a></body></html>"
                );
                root.write(dest, html.as_bytes())
            }
        });
    }

    let lines = errlog.into_inner();
    let mut log = String::new();
    for line in &lines {
        eprintln!("{line}");
        log.push_str(line);
        log.push('\n');
    }
    // Only written when something failed (a clean dump leaves no log, like
    // upstream). The log is untrusted filesystem output too: never open it
    // by an absolute path, follow an existing link, or suppress a write
    // failure.
    if !log.is_empty() {
        root.write(std::path::Path::new("dump_errors.log"), log.as_bytes())?;
    }
    if errors.into_inner() > 0 {
        Ok(ExitCode::from(2))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

struct DumpPlan {
    exiled: Vec<bool>,
    fixed_fallbacks: bool,
    temp_namespace: usize,
    root: Option<DumpRoot>,
}

fn plan_dump<'a>(
    paths: impl Iterator<Item = &'a str> + Clone,
    dir: &Path,
    exceptions: &Path,
) -> Result<DumpPlan, Error> {
    let mut natural = HashSet::new();
    let mut fixed_fallbacks = false;
    let mut temp_namespaces = HashSet::new();
    let temp_prefix = format!(".zimdump-{}-", std::process::id());
    for rel in paths.clone() {
        if !natural.insert(rel) {
            return Err(io_err(&format!("ambiguous dump destination: {rel}")));
        }
        let path = Path::new(rel);
        if path.starts_with("dump_errors.log") {
            return Err(io_err(&format!(
                "dump destination conflicts with dump_errors.log: {rel}"
            )));
        }
        fixed_fallbacks |= path.starts_with(exceptions);
        for component in rel.split('/') {
            if let Some((namespace, _)) = component
                .strip_prefix(&temp_prefix)
                .and_then(|s| s.split_once('-'))
            {
                if let Ok(n) = namespace.parse::<usize>() {
                    temp_namespaces.insert(n);
                }
            }
        }
    }
    // No archive entry or required directory can share a publication temp
    // name. Unrelated pre-existing temp names are handled by O_EXCL.
    let temp_namespace = (0..).find(|n| !temp_namespaces.contains(n)).unwrap();
    let root = match DumpRoot::open(dir, temp_namespace, false) {
        Ok(root) => Some(root),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let mut exiled = Vec::new();
    let mut destinations = HashSet::new();
    for rel in paths {
        // A shallow file keeps its name; nested archive entries are exiled.
        let path = Path::new(rel);
        let mut fallback = path
            .ancestors()
            .skip(1)
            .any(|parent| parent.to_str().is_some_and(|p| natural.contains(p)));
        // Literal entries in the exception namespace can coincide with the
        // actual fallback for a pre-existing filesystem conflict. Determine
        // these routes read-only and freeze them before publication.
        if fixed_fallbacks && !fallback {
            if let Some(root) = &root {
                fallback = root.needs_fallback(path)?;
            }
        }
        if fixed_fallbacks {
            let dest = if fallback {
                exception_dest(exceptions, rel)
            } else {
                path.to_owned()
            };
            if let Some(dest) = destinations.replace(dest) {
                return Err(io_err(&format!(
                    "ambiguous dump destination: {}",
                    dest.display()
                )));
            }
        }
        exiled.push(fallback);
    }
    let conflicting_parent = destinations
        .iter()
        .flat_map(|dest| dest.ancestors().skip(1))
        .filter(|parent| destinations.contains(*parent))
        .min();
    if let Some(parent) = conflicting_parent {
        return Err(io_err(&format!(
            "ambiguous dump destination: {} is both a file and a directory",
            parent.display()
        )));
    }
    Ok(DumpPlan {
        exiled,
        fixed_fallbacks,
        temp_namespace,
        root,
    })
}

/// A directory capability: all archive-controlled traversal uses `openat`
/// with `O_NOFOLLOW`, and publication replaces directory entries atomically.
/// Keeping directory descriptors open avoids check-then-follow races when
/// a parent path is replaced with a symlink. Replacing (rather than
/// truncating) final files also leaves external hard-link targets intact.
#[cfg(unix)]
struct DumpRoot(std::os::fd::OwnedFd, usize);

#[cfg(unix)]
impl DumpRoot {
    const DIR_FLAGS: rustix::fs::OFlags = rustix::fs::OFlags::RDONLY
        .union(rustix::fs::OFlags::DIRECTORY)
        .union(rustix::fs::OFlags::NOFOLLOW)
        .union(rustix::fs::OFlags::CLOEXEC);

    fn open(path: &Path, temp_namespace: usize, create: bool) -> io::Result<Self> {
        // The caller may select a symlinked root. Resolve it once, then walk
        // every archive-controlled component relative to this descriptor.
        if create {
            std::fs::create_dir_all(path)?;
        }
        let path = std::fs::canonicalize(path)?;
        Ok(Self(
            rustix::fs::open(path, Self::DIR_FLAGS, rustix::fs::Mode::empty())?,
            temp_namespace,
        ))
    }

    fn parent<'a>(
        &self,
        path: &'a Path,
        create: bool,
    ) -> io::Result<(std::os::fd::OwnedFd, &'a std::ffi::OsStr)> {
        use rustix::fs::{mkdirat, openat, Mode};
        use rustix::io::Errno;
        use std::path::Component;

        let mut parts = path.components().peekable();
        let mut dir = self.0.try_clone()?;
        while let Some(part) = parts.next() {
            let Component::Normal(name) = part else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "non-relative dump path",
                ));
            };
            if parts.peek().is_none() {
                return Ok((dir, name));
            }
            let open = || openat(&dir, name, Self::DIR_FLAGS, Mode::empty());
            dir = match open() {
                Ok(next) => next,
                Err(Errno::NOENT) if create => {
                    match mkdirat(&dir, name, Mode::from_raw_mode(0o777)) {
                        Ok(()) | Err(Errno::EXIST) => {}
                        Err(error) => return Err(error.into()),
                    }
                    // Another process may have won mkdirat. Reopen with the
                    // same no-follow flags so a link can never become a parent.
                    open()?
                }
                Err(error) => return Err(error.into()),
            };
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty dump path",
        ))
    }

    fn needs_fallback(&self, path: &Path) -> io::Result<bool> {
        use rustix::fs::{statat, AtFlags, FileType};
        let (parent, name) = match self.parent(path, false) {
            Ok(parent) => parent,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) if is_collision(&e) => return Ok(true),
            Err(e) => return Err(e),
        };
        match statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => Ok(FileType::from_raw_mode(stat.st_mode) == FileType::Directory),
            Err(rustix::io::Errno::NOENT) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    fn publish(
        &self,
        path: &std::path::Path,
        create: impl Fn(&std::os::fd::OwnedFd, &str) -> io::Result<()>,
    ) -> io::Result<()> {
        use rustix::fs::{renameat, unlinkat, AtFlags};
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT: AtomicU64 = AtomicU64::new(0);
        let (parent, name) = self.parent(path, true)?;
        loop {
            let temp = format!(
                ".zimdump-{}-{}-{}",
                std::process::id(),
                self.1,
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            match create(&parent, &temp) {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                result => result?,
            }
            if let Err(error) = renameat(&parent, temp.as_str(), &parent, name) {
                let _ = unlinkat(&parent, temp.as_str(), AtFlags::empty());
                return Err(error.into());
            }
            return Ok(());
        }
    }

    fn write(&self, path: &std::path::Path, data: &[u8]) -> io::Result<()> {
        use rustix::fs::{openat, unlinkat, AtFlags, Mode, OFlags};

        self.publish(path, |parent, name| {
            let fd = openat(
                parent,
                name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o666),
            )?;
            let mut file = std::fs::File::from(fd);
            let result = file.write_all(data);
            if result.is_err() {
                let _ = unlinkat(parent, name, AtFlags::empty());
            }
            result
        })
    }

    fn symlink(&self, path: &std::path::Path, target: &str) -> io::Result<()> {
        self.publish(path, |parent, name| {
            Ok(rustix::fs::symlinkat(target, parent, name)?)
        })
    }
}

// std::fs path-based writes cannot guarantee confinement against Windows
// reparse-point races. Refuse extraction rather than claim a lexical path
// filter provides that guarantee (or silently omit --redirect output).
#[cfg(not(unix))]
struct DumpRoot;

#[cfg(not(unix))]
impl DumpRoot {
    fn needs_fallback(&self, _: &Path) -> io::Result<bool> {
        Err(io::ErrorKind::Unsupported.into())
    }

    fn open(_: &Path, _: usize, _: bool) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "root-confined extraction is not supported on this platform",
        ))
    }

    fn write(&self, _: &std::path::Path, _: &[u8]) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "root-confined extraction is not supported on this platform",
        ))
    }

    fn symlink(&self, _: &std::path::Path, _: &str) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "root-confined extraction is not supported on this platform",
        ))
    }
}

/// Express a root-relative target from the directory containing `dest`.
fn relative_target(dest: &std::path::Path, target: &str) -> String {
    let depth = dest.components().count().saturating_sub(1);
    let mut out = "../".repeat(depth);
    if depth == 0 {
        out.push_str("./");
    }
    out.push_str(target);
    out
}

/// Encode filesystem path bytes as a URL, not a query/fragment or scheme.
fn redirect_url(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            out.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            write!(out, "%{byte:02X}").unwrap();
        }
    }
    out
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// True for errors caused by a file-vs-directory path collision:
/// a parent component exists as a regular file (`NotADirectory` /
/// `AlreadyExists` from create_dir_all) or the destination itself
/// exists as a directory (`IsADirectory`).
fn is_collision(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::AlreadyExists | io::ErrorKind::NotADirectory | io::ErrorKind::IsADirectory
    )
}

/// `DIR/_exceptions/<escaped-rel-path>` — the fallback location for
/// entries whose natural path collides. `/` is escaped as `%2f` (and
/// `%` as `%25`) so the entry lands as a single flat file, matching
/// upstream zimdump.
fn exception_dest(exceptions_dir: &std::path::Path, rel: &str) -> PathBuf {
    let mut esc = String::with_capacity(rel.len());
    for c in rel.chars() {
        match c {
            '%' => esc.push_str("%25"),
            '/' => esc.push_str("%2f"),
            _ => esc.push(c),
        }
    }
    // Escaping `/` stops multi-segment traversal, but a path that is
    // exactly "." or ".." survives it and would name the exceptions dir
    // or its parent. Neither is a file we may write.
    if esc.is_empty() || esc == "." || esc == ".." {
        esc.insert(0, '_');
    }
    exceptions_dir.join(esc)
}

/// Sanitize an entry path for use relative to the dump root. Entry
/// paths come from the (untrusted) archive, so rebuild from the
/// segments, dropping empty ones, `.` and `..` — otherwise a crafted
/// path like `../../home/user/.bashrc` would traverse out of `--dir`
/// and overwrite arbitrary files.
///
/// On Windows, `\` is also a separator and `:` marks a drive-absolute
/// path (`PathBuf::join("C:\\…")` REPLACES the dump root entirely), so
/// both are neutralized there too. On Unix they are ordinary filename
/// bytes and are left alone to keep dump fidelity for real archive
/// paths like `Category:Foo`.
fn safe_path(p: &str) -> String {
    let is_sep = |c: char| c == '/' || (cfg!(windows) && c == '\\');
    let out: Vec<&str> = p
        .split(is_sep)
        .filter(|seg| {
            !seg.is_empty() && *seg != "." && *seg != ".." && !(cfg!(windows) && seg.contains(':'))
        })
        .collect();
    if out.is_empty() {
        // A path made entirely of traversal segments still needs a
        // filename so the entry dumps somewhere inside the root.
        "_".to_string()
    } else {
        out.join("/")
    }
}

// ---------------- helpers ----------------

fn io_err(msg: &str) -> Error {
    Error::Io(io::Error::new(io::ErrorKind::InvalidInput, msg.to_string()))
}

fn hex_lower(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", x);
    }
    s
}

fn format_uuid(u: &[u8; 16]) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        u[0], u[1], u[2], u[3], u[4], u[5], u[6], u[7], u[8], u[9], u[10], u[11], u[12], u[13], u[14], u[15],
    )
}
