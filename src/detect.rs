//! Smarter binary/plain-text detection.
//!
//! The previous implementation only scanned the first 1024 bytes for a `NUL`
//! byte. That heuristic has two well-known problems:
//!
//! 1. **False positives** (binary classified as text): a binary file that
//!    happens to have no `NUL` in its first 1024 bytes slips through and gets
//!    fed to the line-ending rewriter, which can corrupt it.
//! 2. **False negatives** (text classified as binary): UTF-16 text is full of
//!    `NUL` bytes but is clearly text.
//!
//! This module combines several cheap signals, in priority order:
//!
//! * Sample size bumped to 8 KiB (matches `ripgrep` / `git`).
//! * Recognised BOM (`UTF-8`, `UTF-16LE`, `UTF-16BE`, `UTF-32LE`, `UTF-32BE`) →
//!   text, regardless of `NUL` bytes.
//! * Any `NUL` byte in the sample → binary (catches virtually all real
//!   binaries; UTF-16/32 already handled by the BOM check above).
//! * Otherwise compute the ratio of "suspicious" control bytes (control chars
//!   that are not common whitespace) — a ratio above 30% is treated as binary.
//!   This catches the "no NUL but still binary" case (e.g. some compressed /
//!   random data, exotic encodings).
//! * Empty sample → treated as text (empty files are no-ops for the caller).

/// Result of inspecting a byte sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentType {
    /// Sample was empty (e.g. a 0-byte file).
    Empty,
    /// Sample looks like plain text (possibly UTF-16/UTF-32 with BOM).
    Text,
    /// Sample looks like binary data.
    Binary,
}

/// Number of leading bytes used to classify a file.
///
/// Matches the choice made by `git` and `ripgrep`. Larger samples slightly
/// reduce false positives at the cost of more I/O on binaries; 8 KiB is a
/// well-tested sweet spot.
pub const SAMPLE_SIZE: usize = 8 * 1024;

/// Maximum ratio (in `[0.0, 1.0]`) of "suspicious" control bytes allowed
/// before a sample without `NUL` is still classified as binary.
const BINARY_CONTROL_RATIO: f64 = 0.30;

/// Classify a (possibly partial) byte sample.
///
/// Only the first [`SAMPLE_SIZE`] bytes are inspected; callers may pass the
/// whole file, this function will not look past the limit.
#[must_use]
pub fn detect_content(sample: &[u8]) -> ContentType {
    let sample = if sample.len() > SAMPLE_SIZE {
        &sample[..SAMPLE_SIZE]
    } else {
        sample
    };

    if sample.is_empty() {
        return ContentType::Empty;
    }

    // BOM-encodings are text by definition, even though UTF-16/32 contain NUL.
    if has_text_bom(sample) {
        return ContentType::Text;
    }

    // A NUL byte in a non-BOM-encoded file is the strongest "binary" signal.
    if sample.contains(&0x00) {
        return ContentType::Binary;
    }

    // Fall back to a ratio of "suspicious" control bytes. This catches binary
    // samples that simply happen to have no NUL in their first 8 KiB.
    if suspicious_ratio(sample) > BINARY_CONTROL_RATIO {
        return ContentType::Binary;
    }

    ContentType::Text
}

/// Returns `true` iff the sample starts with a BOM that identifies a text
/// encoding (UTF-8 / UTF-16 / UTF-32, either endianness).
fn has_text_bom(sample: &[u8]) -> bool {
    const UTF8: &[u8] = &[0xEF, 0xBB, 0xBF];
    const UTF16_LE: &[u8] = &[0xFF, 0xFE];
    const UTF16_BE: &[u8] = &[0xFE, 0xFF];
    const UTF32_LE: &[u8] = &[0xFF, 0xFE, 0x00, 0x00];
    const UTF32_BE: &[u8] = &[0x00, 0x00, 0xFE, 0xFF];

    // UTF-32LE starts with the UTF-16LE BOM, so check the longer one first.
    sample.starts_with(UTF32_LE)
        || sample.starts_with(UTF32_BE)
        || sample.starts_with(UTF8)
        || sample.starts_with(UTF16_LE)
        || sample.starts_with(UTF16_BE)
}

/// Fraction of bytes in `sample` that are control characters *other than*
/// the common ASCII whitespace bytes (`\t \n \r \f`).
fn suspicious_ratio(sample: &[u8]) -> f64 {
    // Avoid the `as` cast warning from pedantic clippy on empty slices; the
    // caller already returns early on empty input, so this branch is only
    // reached with `len >= 1`.
    let total = sample.len();
    let suspicious = sample.iter().filter(|&&b| is_suspicious(b)).count();
    #[expect(clippy::cast_precision_loss)]
    {
        suspicious as f64 / total as f64
    }
}

/// A byte is "suspicious" if it is a non-whitespace C0 control, or any C1
/// control byte (0x7F, 0x80..=0x9F). Common text whitespace is allowed.
const fn is_suspicious(b: u8) -> bool {
    match b {
        b'\t' | b'\n' | b'\r' | 0x0C => false,    // \t \n \r \f
        0x00..=0x1F | 0x7F | 0x80..=0x9F => true, // other C0 controls, DEL, C1
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_sample_is_empty() {
        assert_eq!(detect_content(b""), ContentType::Empty);
    }

    #[test]
    fn plain_ascii_is_text() {
        assert_eq!(detect_content(b"hello world"), ContentType::Text);
    }

    #[test]
    fn plain_utf8_is_text() {
        assert_eq!(detect_content("héllo, 世界".as_bytes()), ContentType::Text);
    }

    #[test]
    fn null_byte_is_binary() {
        assert_eq!(detect_content(b"abc\x00def"), ContentType::Binary);
    }

    #[test]
    fn only_null_is_binary() {
        assert_eq!(detect_content(&[0x00]), ContentType::Binary);
    }

    #[test]
    fn utf8_bom_is_text_even_with_control_bytes() {
        let mut sample = vec![0xEF, 0xBB, 0xBF];
        sample.extend_from_slice(b"\x01\x02\x03 hello");
        assert_eq!(detect_content(&sample), ContentType::Text);
    }

    #[test]
    fn utf16_le_bom_is_text() {
        // "hi" in UTF-16LE: contains NUL bytes but the BOM wins.
        let sample = [0xFF, 0xFE, b'h', 0x00, b'i', 0x00];
        assert_eq!(detect_content(&sample), ContentType::Text);
    }

    #[test]
    fn utf16_be_bom_is_text() {
        let sample = [0xFE, 0xFF, 0x00, b'h', 0x00, b'i'];
        assert_eq!(detect_content(&sample), ContentType::Text);
    }

    #[test]
    fn utf32_le_bom_is_text() {
        let sample = [0xFF, 0xFE, 0x00, 0x00, b'h', 0x00, 0x00, 0x00];
        assert_eq!(detect_content(&sample), ContentType::Text);
    }

    #[test]
    fn many_control_bytes_without_null_is_binary() {
        // 100 bytes of \x01 → 100% suspicious ratio.
        let sample = vec![0x01; 100];
        assert_eq!(detect_content(&sample), ContentType::Binary);
    }

    #[test]
    fn few_control_bytes_are_tolerated() {
        // 10% suspicious ratio — below the 30% threshold.
        let mut sample = vec![b'.'; 90];
        sample.extend(std::iter::repeat_n(0x01, 10));
        assert_eq!(detect_content(&sample), ContentType::Text);
    }

    #[test]
    fn binary_sample_without_null_is_caught_by_ratio() {
        // Pseudo-random-looking bytes in the C1 control range: no NUL, but
        // very high suspicious ratio.
        let sample: Vec<u8> = (0..200).map(|i| 0x80 + (i % 32)).collect();
        assert_eq!(detect_content(&sample), ContentType::Binary);
    }

    #[test]
    fn sample_is_truncated_to_sample_size() {
        // A sample larger than SAMPLE_SIZE must be truncated, not panicked.
        let big: Vec<u8> = vec![b'a'; SAMPLE_SIZE * 4];
        assert_eq!(detect_content(&big), ContentType::Text);

        // A NUL byte within the first SAMPLE_SIZE bytes is detected.
        let mut bad: Vec<u8> = vec![b'a'; SAMPLE_SIZE];
        bad[SAMPLE_SIZE - 1] = 0x00;
        assert_eq!(detect_content(&bad), ContentType::Binary);

        // A NUL byte *beyond* the sample window is invisible to the
        // detector — this is the whole point of sampling.
        let mut hidden: Vec<u8> = vec![b'a'; SAMPLE_SIZE];
        hidden.push(0x00);
        assert_eq!(detect_content(&hidden), ContentType::Text);
    }

    #[test]
    fn windows_crlf_is_text() {
        assert_eq!(detect_content(b"a\r\nb\r\nc"), ContentType::Text);
    }

    #[test]
    fn form_feed_and_tab_are_not_suspicious() {
        let sample = [b'\t', b'\n', b'\r', 0x0C, b'a'];
        assert_eq!(detect_content(&sample), ContentType::Text);
    }
}
