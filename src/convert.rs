//! Byte-level CRLF → LF conversion with atomic write-back.

use std::{
    fs,
    io::{self, Write},
    path::Path,
};

use crate::detect::{ContentType, SAMPLE_SIZE, detect_content};

/// Caller-controlled options for [`convert_path`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConvertOptions {
    /// If `true`, only report what *would* be done — do not touch the disk.
    pub dry_run: bool,
}

/// Outcome of a single-file conversion attempt.
///
/// `Ok(...)` always means "no I/O error happened"; the variant tells the
/// caller whether the file actually changed on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConvertOutcome {
    /// The file was rewritten (or, under `dry_run`, *would* have been).
    Converted,
    /// The file is text but already LF-only — nothing to do.
    Already,
    /// The file was classified as binary / empty and left untouched.
    SkippedBinary,
}

/// Read, classify, and — if needed — atomically rewrite `path` so that every
/// `CRLF` becomes `LF`.
///
/// # Errors
///
/// Returns `Err` only for genuine I/O failures (read, temp-file creation,
/// write, persist, permissions copy). A file being classified as binary is
/// *not* an error and is reported via [`ConvertOutcome::SkippedBinary`].
///
/// # Atomicity
///
/// Writes go to a `tempfile::NamedTempFile` in the same directory as `path`,
/// then renamed over the target via [`tempfile::NamedTempFile::persist`].
/// On Unix the original file's permission bits are copied onto the temp file
/// before the rename, so the executable bit (and any other mode bits) survive.
pub fn convert_path(path: &Path, opts: &ConvertOptions) -> io::Result<ConvertOutcome> {
    // Single read: the first SAMPLE_SIZE bytes are used for content sniffing,
    // the rest is needed for the actual rewrite. Reading into a Vec<u8> avoids
    // the double-read of the previous implementation and also lets us operate
    // on arbitrary bytes (not just valid UTF-8).
    let bytes = fs::read(path)?;

    // Sniff only the leading SAMPLE_SIZE bytes; that's what detect_content
    // looks at anyway, so we can pass the whole buffer safely.
    let kind = if bytes.len() <= SAMPLE_SIZE {
        detect_content(&bytes)
    } else {
        detect_content(&bytes[..SAMPLE_SIZE])
    };

    match kind {
        ContentType::Empty | ContentType::Binary => {
            return Ok(ConvertOutcome::SkippedBinary);
        }
        ContentType::Text => {}
    }

    if !contains_crlf(&bytes) {
        return Ok(ConvertOutcome::Already);
    }

    if opts.dry_run {
        return Ok(ConvertOutcome::Converted);
    }

    let rewritten = replace_crlf(&bytes);
    atomic_write_with_mode(path, &rewritten)?;
    Ok(ConvertOutcome::Converted)
}

/// Cheap "does this buffer contain `\r\n`" check.
///
/// Implemented manually rather than via `memchr::memmem` to keep the
/// dependency surface small; a single linear scan is fast enough in practice
/// and we have to scan anyway for the rewrite.
fn contains_crlf(bytes: &[u8]) -> bool {
    bytes.windows(2).any(|w| w == b"\r\n")
}

/// Allocate a new buffer with every `CRLF` replaced by `LF`.
///
/// Lone `\r` (old Mac style) are preserved intentionally — converting those
/// is out of scope for a tool literally named `lf`.
fn replace_crlf(input: &[u8]) -> Vec<u8> {
    // Worst case: no CRLF at all → same size. Best case: all CRLF → half size.
    // Allocating to `input.len()` is a tight upper bound.
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if i + 1 < input.len() && input[i] == b'\r' && input[i + 1] == b'\n' {
            out.push(b'\n');
            i += 2;
        } else {
            out.push(input[i]);
            i += 1;
        }
    }
    out
}

/// Write `data` to a temp file in `path`'s directory, copy the original's
/// permission bits on Unix, then atomically rename over `path`.
fn atomic_write_with_mode(path: &Path, data: &[u8]) -> io::Result<()> {
    use tempfile::NamedTempFile;

    let parent = path.parent().unwrap_or_else(|| Path::new("."));

    let mut tmp = NamedTempFile::new_in(parent)?;
    tmp.write_all(data)?;
    tmp.flush()?;

    // Preserve the original's permission bits. On Windows this is a no-op
    // (the rename replaces the target and ACLs are not touched).
    copy_mode(path, tmp.as_ref())?;

    // `persist` is the atomic rename. It errors if the target already exists
    // *and* can't be replaced; on Windows Rust uses `MoveFileExW` with
    // `MOVEFILE_REPLACE_EXISTING`, on Unix it's `rename(2)`.
    tmp.persist(path)
        .map_err(|e| io::Error::other(e.to_string()))?;

    Ok(())
}

/// Copy the original file's permission bits onto the temp file. On Windows
/// this is a no-op (the rename replaces the target and ACLs are untouched).
#[cfg_attr(not(unix), expect(clippy::unnecessary_wraps))]
fn copy_mode(src: &Path, dst: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(src)?.permissions().mode();
        fs::set_permissions(dst, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (src, dst);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("create temp dir")
    }

    fn read_all(path: &Path) -> Vec<u8> {
        let mut f = fs::File::open(path).expect("open file");
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).expect("read file");
        buf
    }

    #[test]
    fn contains_crlf_basic() {
        assert!(!contains_crlf(b""));
        assert!(!contains_crlf(b"abc"));
        assert!(!contains_crlf(b"abc\r"));
        assert!(!contains_crlf(b"abc\n"));
        assert!(contains_crlf(b"a\r\nb"));
        assert!(contains_crlf(b"\r\n"));
    }

    #[test]
    fn replace_keeps_lone_cr() {
        assert_eq!(replace_crlf(b"a\rb"), b"a\rb");
    }

    #[test]
    fn replace_converts_crlf() {
        assert_eq!(replace_crlf(b"a\r\nb\r\nc"), b"a\nb\nc");
    }

    #[test]
    fn replace_handles_trailing_crlf() {
        assert_eq!(replace_crlf(b"a\r\n"), b"a\n");
    }

    #[test]
    fn replace_handles_lf_lf() {
        assert_eq!(replace_crlf(b"a\n\nb"), b"a\n\nb");
    }

    #[test]
    fn replace_handles_lone_cr_then_lf() {
        // "\r\r\n" -> "\r\n" (only the last two form a CRLF)
        assert_eq!(replace_crlf(b"a\r\r\nb"), b"a\r\nb");
    }

    #[test]
    fn dry_run_does_not_touch_disk() {
        let dir = temp_dir();
        let path = dir.path().join("a.txt");
        fs::write(&path, b"a\r\nb\r\nc").unwrap();

        let out = convert_path(&path, &ConvertOptions { dry_run: true }).unwrap();
        assert_eq!(out, ConvertOutcome::Converted);

        // File on disk must be unchanged.
        assert_eq!(read_all(&path), b"a\r\nb\r\nc");
    }

    #[test]
    fn empty_file_is_skipped() {
        let dir = temp_dir();
        let path = dir.path().join("empty");
        fs::write(&path, b"").unwrap();

        let out = convert_path(&path, &ConvertOptions::default()).unwrap();
        assert_eq!(out, ConvertOutcome::SkippedBinary);
        assert_eq!(read_all(&path), b"");
    }

    #[test]
    fn binary_file_is_skipped() {
        let dir = temp_dir();
        let path = dir.path().join("bin");
        fs::write(&path, b"abc\x00\x01\x02").unwrap();

        let out = convert_path(&path, &ConvertOptions::default()).unwrap();
        assert_eq!(out, ConvertOutcome::SkippedBinary);
        assert_eq!(read_all(&path), b"abc\x00\x01\x02");
    }

    #[test]
    fn already_lf_is_noop() {
        let dir = temp_dir();
        let path = dir.path().join("a.txt");
        fs::write(&path, b"a\nb\nc").unwrap();
        let mtime_before = fs::metadata(&path).unwrap().modified().unwrap();

        let out = convert_path(&path, &ConvertOptions::default()).unwrap();
        assert_eq!(out, ConvertOutcome::Already);
        assert_eq!(read_all(&path), b"a\nb\nc");

        let mtime_after = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(mtime_before, mtime_after, "must not rewrite unchanged file");
    }

    #[test]
    fn converts_crlf_file() {
        let dir = temp_dir();
        let path = dir.path().join("a.txt");
        fs::write(&path, b"a\r\nb\r\nc").unwrap();

        let out = convert_path(&path, &ConvertOptions::default()).unwrap();
        assert_eq!(out, ConvertOutcome::Converted);
        assert_eq!(read_all(&path), b"a\nb\nc");
    }

    #[test]
    fn preserves_lone_cr() {
        let dir = temp_dir();
        let path = dir.path().join("a.txt");
        fs::write(&path, b"a\rb\r\nc").unwrap();

        let out = convert_path(&path, &ConvertOptions::default()).unwrap();
        assert_eq!(out, ConvertOutcome::Converted);
        assert_eq!(read_all(&path), b"a\rb\nc");
    }

    #[cfg(unix)]
    #[test]
    fn preserves_executable_bit() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir();
        let path = dir.path().join("script.sh");
        fs::write(&path, b"#!/bin/sh\r\necho hi\r\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();

        let out = convert_path(&path, &ConvertOptions::default()).unwrap();
        assert_eq!(out, ConvertOutcome::Converted);

        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o755,
            "executable bit must survive the atomic rewrite"
        );
    }

    #[test]
    fn works_for_non_utf8_text() {
        // Latin-1 bytes — invalid UTF-8 sequence `\xff`, but no NUL and
        // a low suspicious ratio, so the previous read_to_string would have
        // errored while we happily process it byte-wise.
        let dir = temp_dir();
        let path = dir.path().join("latin1.txt");
        fs::write(&path, b"\xff\xe9\r\nhello\r\n").unwrap();

        let out = convert_path(&path, &ConvertOptions::default()).unwrap();
        assert_eq!(out, ConvertOutcome::Converted);
        assert_eq!(read_all(&path), b"\xff\xe9\nhello\n");
    }
}
