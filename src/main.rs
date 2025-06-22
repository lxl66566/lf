use palc::Parser;
use rayon::prelude::*;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use walkdir::WalkDir;

/// A high-performance tool for recursively converting line endings of all text files in a folder to LF.
#[derive(Parser, Debug)]
struct Args {
    /// The path to the folder to process
    path: Option<PathBuf>,
}

fn main() {
    let args = Args::parse();
    let root_path = args.path.unwrap_or_else(|| ".".into());

    if root_path.is_dir() {
        let start_time = Instant::now();
        let processed_count = AtomicUsize::new(0);
        let skipped_count = AtomicUsize::new(0);
        let error_count = AtomicUsize::new(0);
        WalkDir::new(&root_path)
            .into_iter()
            .par_bridge()
            .for_each(|entry_result| {
                let entry = match entry_result {
                    Ok(entry) => entry,
                    Err(e) => {
                        eprintln!("Error: Failed to traverse directory: {}", e);
                        error_count.fetch_add(1, Ordering::SeqCst);
                        return;
                    }
                };

                let path = entry.path();
                if path.is_file() {
                    match process_file(path) {
                        Ok(true) => {
                            processed_count.fetch_add(1, Ordering::SeqCst);
                            println!("Processed: {}", path.display());
                        }
                        Ok(false) => {
                            skipped_count.fetch_add(1, Ordering::SeqCst);
                        }
                        Err(e) => {
                            error_count.fetch_add(1, Ordering::SeqCst);
                            eprintln!("Error processing file {}: {}", path.display(), e);
                        }
                    }
                }
            });

        let duration = start_time.elapsed();
        println!("\n--- Processing Complete ---");
        println!(
            "Files successfully converted: {}",
            processed_count.load(Ordering::SeqCst)
        );
        println!("Files skipped: {}", skipped_count.load(Ordering::SeqCst));
        println!("Errors encountered: {}", error_count.load(Ordering::SeqCst));
        println!("Total time: {:?}", duration);
    } else if root_path.is_file() {
        let path = root_path.as_path();
        match process_file(path) {
            Ok(true) => {
                println!("Processed: {}", path.display());
            }
            Ok(false) => {
                eprintln!("Skipped: {}", path.display());
            }
            Err(e) => {
                eprintln!("Error processing file {}: {}", path.display(), e);
            }
        }
    }
}

/// Processes a single file, converting line endings.
///
/// Returns:
/// - `Ok(true)` if the file was successfully converted.
/// - `Ok(false)` if the file was skipped (not a text file or already LF).
/// - `Err(io::Error)` if an I/O error occurs.
fn process_file(path: &Path) -> io::Result<bool> {
    // 1. Check if it's likely a text file
    if !is_likely_text_file(path)? {
        return Ok(false);
    }

    // 2. Read file content
    let content = fs::read_to_string(path)?;

    // 3. Check if it contains CRLF
    if !content.contains("\r\n") {
        return Ok(false);
    }

    // 4. Replace CRLF with LF
    let new_content = content.replace("\r\n", "\n");

    // 5. Write back to file
    fs::write(path, new_content)?;

    Ok(true)
}

/// Determines if a file is likely a text file by reading the first 1024 bytes and checking for NULL bytes.
/// This is a heuristic method, effective for most cases.
fn is_likely_text_file(path: &Path) -> io::Result<bool> {
    let mut file = fs::File::open(path)?;
    let mut buffer = [0; 1024];
    let n = file.read(&mut buffer)?;

    // Check if the buffer contains NULL bytes (0x00)
    Ok(!buffer[..n].contains(&0))
}
