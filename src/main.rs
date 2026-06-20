//! `lf` binary: parallel CRLF → LF converter.
//!
//! Walks a directory tree using the `ignore` crate (which honours
//! `.gitignore` / `.ignore` / `.git/info/exclude` / global gitignore by
//! default), classifies each candidate via [`lf::detect`], and atomically
//! rewrites those containing CRLF via [`lf::convert`]. Parallelism comes
//! from `ignore`'s built-in worker pool — the conversion closure runs on
//! every worker thread.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

use ignore::{DirEntry, WalkBuilder, WalkState};
use lf::{ConvertOptions, ConvertOutcome, convert_path};
use palc::Parser;

/// A high-performance tool for recursively converting line endings of all
/// text files in a folder to LF.
#[derive(Parser, Debug)]
#[expect(clippy::struct_excessive_bools)]
struct Args {
    /// The path to process. May be a file or a directory. Defaults to the
    /// current directory.
    path: Option<PathBuf>,

    /// Do not write anything; only print what *would* be converted.
    #[arg(short = 'n', long)]
    dry_run: bool,

    /// Suppress per-file output. Summary on stderr is still printed.
    #[arg(short, long, conflicts_with = "verbose")]
    quiet: bool,

    /// Print every visited file, including ones that were skipped.
    #[arg(short, long, conflicts_with = "quiet")]
    verbose: bool,

    /// Disable all of `.gitignore` / `.ignore` / `.git/info/exclude` /
    /// global gitignore filtering. The `.git` directory itself is still
    /// always skipped.
    #[arg(long)]
    no_gitignore: bool,

    /// Skip hidden files and directories (Unix dotfiles). By default `lf`
    /// processes hidden files, matching the behaviour of the previous
    /// release.
    #[arg(long)]
    no_hidden: bool,

    /// Maximum recursion depth. `1` means only the direct children of the
    /// root path.
    #[arg(long)]
    max_depth: Option<usize>,

    /// Comma-separated list of extensions to *keep* (others are skipped).
    /// The leading dot is optional and matching is case-insensitive.
    /// Example: `-E rs,md,TOML`.
    #[arg(short = 'E', long, use_value_delimiter = true)]
    ext: Vec<String>,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let root = args.path.clone().unwrap_or_else(|| ".".into());

    if !root.exists() {
        eprintln!("Error: path does not exist: {}", root.display());
        return ExitCode::from(2);
    }

    let ext_filter = build_ext_filter(&args.ext);

    if root.is_file() {
        return run_single_file(&root, &args, &ext_filter);
    }

    run_directory(&root, &args, &ext_filter)
}

/// Run the directory walker: build a parallel `ignore` walker, dispatch
/// every file through [`convert_path`], aggregate counters atomically, and
/// return a non-zero exit code on any error.
fn run_directory(root: &Path, args: &Args, ext_filter: &ExtFilter) -> ExitCode {
    let start = Instant::now();

    let mut builder = WalkBuilder::new(root);
    builder
        // `.gitignore` / `.ignore` / `.git/info/exclude` / global gitignore
        // all toggle together via `standard_filters`.
        .standard_filters(!args.no_gitignore)
        // By default the `ignore` crate skips hidden files. The previous
        // release of `lf` processed them, so we opt back in unless the user
        // explicitly passes `--no-hidden`.
        .hidden(args.no_hidden)
        .follow_links(false)
        // The `.git` directory is always skipped, regardless of any
        // `--no-gitignore` flag — touching git's internal objects is almost
        // never what the user wants. `filter_entry` runs *after* the
        // gitignore match, so it is additive rather than overriding.
        .filter_entry(|entry: &DirEntry| entry.file_name() != OsStr::new(".git"));
    if let Some(depth) = args.max_depth {
        builder.max_depth(Some(depth));
    }

    let stats = Arc::new(Stats::default());
    let worker = Worker {
        opts: ConvertOptions {
            dry_run: args.dry_run,
        },
        verbose: args.verbose,
        quiet: args.quiet,
        dry_run: args.dry_run,
        ext_filter: ext_filter.clone(),
        stats: Arc::clone(&stats),
    };

    builder.build_parallel().run(move || {
        let worker = worker.clone();
        Box::new(move |result| worker.handle(result))
    });

    let elapsed = start.elapsed();

    // Summary goes to stderr so that `lf . > list.txt` only collects the
    // per-file "Converted:" lines.
    eprintln!(
        "\n--- {} ---",
        if args.dry_run {
            "Dry run summary"
        } else {
            "Processing complete"
        }
    );
    eprintln!(
        "Files converted:     {}",
        stats.converted.load(Ordering::Relaxed)
    );
    eprintln!(
        "Already LF:          {}",
        stats.already.load(Ordering::Relaxed)
    );
    eprintln!(
        "Skipped (binary):    {}",
        stats.skipped_binary.load(Ordering::Relaxed)
    );
    eprintln!(
        "Skipped (extension): {}",
        stats.skipped_ext.load(Ordering::Relaxed)
    );
    eprintln!(
        "Errors:              {}",
        stats.errors.load(Ordering::Relaxed)
    );
    eprintln!("Total time:          {elapsed:?}");

    if stats.errors.load(Ordering::Relaxed) > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Per-worker shared state. Cheap to clone (one `Arc` bump + small Vec clone).
#[derive(Clone)]
struct Worker {
    opts: ConvertOptions,
    verbose: bool,
    quiet: bool,
    dry_run: bool,
    ext_filter: ExtFilter,
    stats: Arc<Stats>,
}

impl Worker {
    fn handle(&self, result: Result<DirEntry, ignore::Error>) -> WalkState {
        // Top-level iterator errors (broken symlink, permission denied on a
        // directory, ...). Surface them but keep walking.
        let entry = match result {
            Ok(e) => e,
            Err(e) => {
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
                eprintln!("Error: traversal failed: {e}");
                return WalkState::Continue;
            }
        };

        let path = entry.path();
        if !path.is_file() {
            return WalkState::Continue;
        }

        if !self.ext_filter.allows(path) {
            self.stats.skipped_ext.fetch_add(1, Ordering::Relaxed);
            if self.verbose {
                eprintln!("Skip (ext): {}", path.display());
            }
            return WalkState::Continue;
        }

        match convert_path(path, &self.opts) {
            Ok(ConvertOutcome::Converted) => {
                self.stats.converted.fetch_add(1, Ordering::Relaxed);
                if !self.quiet {
                    let prefix = if self.dry_run {
                        "Would convert"
                    } else {
                        "Converted"
                    };
                    println!("{prefix}: {}", path.display());
                }
            }
            Ok(ConvertOutcome::Already) => {
                self.stats.already.fetch_add(1, Ordering::Relaxed);
                if self.verbose {
                    eprintln!("Already LF: {}", path.display());
                }
            }
            Ok(ConvertOutcome::SkippedBinary) => {
                self.stats.skipped_binary.fetch_add(1, Ordering::Relaxed);
                if self.verbose {
                    eprintln!("Skip (binary): {}", path.display());
                }
            }
            Err(e) => {
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
                eprintln!("Error processing {}: {e}", path.display());
            }
        }

        WalkState::Continue
    }
}

/// Aggregated counters shared across worker threads. The whole struct is
/// shared behind an `Arc` (see [`Worker::stats`]) and each counter is
/// independently atomic.
#[derive(Default)]
struct Stats {
    converted: AtomicUsize,
    already: AtomicUsize,
    skipped_binary: AtomicUsize,
    skipped_ext: AtomicUsize,
    errors: AtomicUsize,
}

/// Single-file mode: skip the walker entirely and just call into the lib.
fn run_single_file(path: &Path, args: &Args, ext_filter: &ExtFilter) -> ExitCode {
    if !ext_filter.allows(path) {
        eprintln!("Skipped (extension): {}", path.display());
        return ExitCode::SUCCESS;
    }

    let opts = ConvertOptions {
        dry_run: args.dry_run,
    };
    match convert_path(path, &opts) {
        Ok(ConvertOutcome::Converted) => {
            if !args.quiet {
                let prefix = if args.dry_run {
                    "Would convert"
                } else {
                    "Converted"
                };
                println!("{prefix}: {}", path.display());
            }
            ExitCode::SUCCESS
        }
        Ok(ConvertOutcome::Already) => {
            if args.verbose {
                eprintln!("Already LF: {}", path.display());
            } else if !args.quiet {
                eprintln!("Skipped (already LF): {}", path.display());
            }
            ExitCode::SUCCESS
        }
        Ok(ConvertOutcome::SkippedBinary) => {
            if !args.quiet || args.verbose {
                eprintln!("Skipped (binary): {}", path.display());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Error processing {}: {e}", path.display());
            ExitCode::from(1)
        }
    }
}

/// Pre-compiled, case-folded set of allowed extensions.
#[derive(Clone)]
struct ExtFilter {
    /// If empty, every extension is allowed.
    allowed: Vec<String>,
}

impl ExtFilter {
    fn allows(&self, path: &Path) -> bool {
        if self.allowed.is_empty() {
            return true;
        }
        match path.extension().and_then(|e| e.to_str()) {
            Some(ext) => {
                let ext = ext.to_lowercase();
                self.allowed.iter().any(|a| a == &ext)
            }
            None => false,
        }
    }
}

fn build_ext_filter(raw: &[String]) -> ExtFilter {
    let allowed = raw
        .iter()
        .map(|s| s.trim().trim_start_matches('.').to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    ExtFilter { allowed }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_filter_allows_all() {
        let f = build_ext_filter(&[]);
        assert!(f.allows(Path::new("a.txt")));
        assert!(f.allows(Path::new("b")));
        assert!(f.allows(Path::new("c.png")));
    }

    #[test]
    fn filter_matches_case_insensitive_and_dotless() {
        let f = build_ext_filter(&["rs".into(), ".MD".into(), " ToMl ".into()]);
        assert!(f.allows(Path::new("a.rs")));
        assert!(f.allows(Path::new("b.md")));
        assert!(f.allows(Path::new("c.MD")));
        assert!(f.allows(Path::new("d.toml")));
        assert!(!f.allows(Path::new("e.txt")));
        assert!(!f.allows(Path::new("f")));
    }

    #[test]
    fn filter_ignores_empty_entries() {
        let f = build_ext_filter(&[String::new(), "rs".into(), "   ".into()]);
        assert!(f.allows(Path::new("a.rs")));
        assert!(!f.allows(Path::new("b.txt")));
    }
}
