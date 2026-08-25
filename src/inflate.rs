//! Choosing between streaming and whole-buffer inflate.
//!
//! Whole-buffer (libdeflate) measured ~14x cheaper on CPU but allocates the
//! full decompressed size. Streaming (flate2) stays around 10 MB. `auto` reads
//! the gzip ISIZE trailer as a hint, but correctness comes from the buffered
//! path growing and finally falling back, not from trusting the hint.
//!
//! `--inflate-budget` is a **hard ceiling in every mode**, not only `auto`:
//! explicit `--inflate=buffer` picks the strategy, never a licence to exceed
//! the budget. Two terms have to fit inside it — the whole compressed file
//! (`inflate_buffered` must `fs::read` it regardless of the eventual
//! decompressed size) and the decompressed output — so both the initial
//! capacity and the retry ceiling are derived from the budget, and a
//! compressed file bigger than the budget is refused before it is ever read
//! into memory.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use crate::cli::InflateMode;

/// Below this, buffering buys nothing over streaming, so it is the floor for
/// both the initial capacity and the effective ceiling — even under a
/// stingy or zero `--inflate-budget`.
const MIN_CAPACITY: u64 = 1 << 16; // 64 KiB

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    Stream,
    Buffer { capacity: usize, ceiling: usize },
}

/// Last four bytes of a gzip stream: uncompressed size modulo 2^32.
pub fn isize_hint(path: &Path) -> Option<u64> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    if len < 18 {
        return None; // shorter than the smallest possible gzip member
    }
    f.seek(SeekFrom::End(-4)).ok()?;
    let mut b = [0u8; 4];
    f.read_exact(&mut b).ok()?;
    Some(u32::from_le_bytes(b) as u64)
}

/// `budget`, floored to `MIN_CAPACITY`. Applied uniformly to the compressed-
/// size guard and to the derived capacity/ceiling, so a zero or tiny budget
/// still leaves room for genuinely small archives to use the fast path
/// instead of being refused outright.
fn effective_ceiling(budget: u64) -> u64 {
    budget.max(MIN_CAPACITY)
}

pub fn choose(mode: InflateMode, path: &Path, budget: u64) -> Strategy {
    match mode {
        InflateMode::Stream => Strategy::Stream,
        InflateMode::Buffer => buffer_within_budget(path, budget, isize_hint(path)),
        InflateMode::Auto => {
            let Some(hint) = isize_hint(path) else { return Strategy::Stream };
            let compressed = std::fs::metadata(path).map(|m| m.len()).unwrap_or(u64::MAX);

            // gzip never shrinks incompressible data by more than a rounding
            // error, so a hint far below the compressed size means the trailer
            // is not describing this whole stream.
            if hint < compressed / 2 {
                return Strategy::Stream;
            }
            // A zero budget or a hint that already outgrows it: don't even
            // try the buffered path. (This is what makes `--inflate-budget 0`
            // mean "never buffer-inflate" in `auto`.)
            if hint == 0 || hint > budget {
                return Strategy::Stream;
            }
            buffer_within_budget(path, budget, Some(hint))
        }
    }
}

/// Build a `Buffer` strategy whose capacity and ceiling are both derived from
/// `budget`, falling back to `Stream` when the compressed file itself would
/// already blow the budget — `inflate_buffered` reads that file whole
/// (`fs::read`) no matter how the decompressed side turns out, so it is the
/// other unbounded term and must be checked before anything is read into
/// memory.
fn buffer_within_budget(path: &Path, budget: u64, hint: Option<u64>) -> Strategy {
    let ceiling = effective_ceiling(budget);
    let compressed = std::fs::metadata(path).map(|m| m.len()).unwrap_or(u64::MAX);
    if compressed > ceiling {
        return Strategy::Stream;
    }
    let capacity = hint.unwrap_or(1 << 20).min(budget).max(MIN_CAPACITY);
    Strategy::Buffer { capacity: capacity as usize, ceiling: ceiling as usize }
}

/// Whole-buffer inflate. Doubles the output buffer and retries while the
/// decompressor reports insufficient space, up to `ceiling` bytes — an
/// absolute byte count derived from `--inflate-budget`, not a multiplier of
/// `capacity`. A lying ISIZE trailer can make this retry a few times, but it
/// can never make it allocate past the budget the operator set: past
/// `ceiling` this returns `Err` instead of growing further, and the caller
/// falls back to streaming.
pub fn inflate_buffered(path: &Path, capacity: usize, ceiling: usize) -> io::Result<Vec<u8>> {
    let raw = std::fs::read(path)?;
    inflate_bytes(&raw, path, capacity, ceiling)
}

/// The retry loop of [`inflate_buffered`], separated from the read so that a
/// caller which has already read the compressed bytes — `archive.rs` does,
/// through a file handle it has run `resource::prepare` on — does not have to
/// open and read the file a second time. `path` is used only for messages.
pub fn inflate_bytes(
    raw: &[u8],
    path: &Path,
    capacity: usize,
    ceiling: usize,
) -> io::Result<Vec<u8>> {
    let mut cap = capacity.max(MIN_CAPACITY as usize);
    let ceiling = ceiling.max(cap);

    loop {
        let mut d = libdeflater::Decompressor::new();
        let mut out = vec![0u8; cap];
        match d.gzip_decompress(raw, &mut out) {
            Ok(n) => {
                out.truncate(n);
                return Ok(out);
            }
            Err(libdeflater::DecompressionError::InsufficientSpace) if cap < ceiling => {
                cap = cap.saturating_mul(4).min(ceiling);
            }
            Err(libdeflater::DecompressionError::InsufficientSpace) => {
                // Not a bug: an undersized budget for this archive is an
                // expected, silent-fallback case, not an error condition. The
                // caller (`archive::open_decoded_with`) already frames this
                // as a fallback, so this message states only the reason.
                return Err(io::Error::other(format!(
                    "the {ceiling}-byte inflate budget would be exceeded"
                )));
            }
            Err(e) => {
                return Err(io::Error::other(format!(
                    "whole-buffer inflate failed for {}: {e:?}",
                    path.display()
                )));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn gz(body: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(body).unwrap();
        e.finish().unwrap()
    }

    /// Deliberately poorly-compressible bytes, so a gzip of `n` of these
    /// stays close to `n` bytes on disk instead of collapsing via run-length
    /// matches the way `vec![b'a'; n]` would.
    fn incompressible(n: usize) -> Vec<u8> {
        let mut s: u64 = 0x2545F4914F6CDD1D;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
                (s >> 33) as u8
            })
            .collect()
    }

    fn tmp(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.gz");
        std::fs::write(&p, bytes).unwrap();
        (d, p)
    }

    #[test]
    fn reads_the_isize_trailer() {
        let body = vec![b'a'; 5000];
        let (_d, p) = tmp(&gz(&body));
        assert_eq!(isize_hint(&p), Some(5000));
    }

    #[test]
    fn isize_of_a_too_short_file_is_none() {
        let (_d, p) = tmp(b"tiny");
        assert_eq!(isize_hint(&p), None);
    }

    #[test]
    fn auto_uses_buffer_when_the_hint_fits_the_budget() {
        let (_d, p) = tmp(&gz(&vec![b'a'; 5000]));
        assert!(matches!(
            choose(crate::cli::InflateMode::Auto, &p, 64 * 1024),
            Strategy::Buffer { .. }
        ));
    }

    #[test]
    fn auto_streams_when_the_hint_exceeds_the_budget() {
        let (_d, p) = tmp(&gz(&vec![b'a'; 200_000]));
        assert!(matches!(
            choose(crate::cli::InflateMode::Auto, &p, 1024),
            Strategy::Stream
        ));
    }

    #[test]
    fn auto_streams_when_the_hint_is_implausible() {
        // ISIZE far smaller than the compressed size means the trailer is not
        // describing this whole stream (wrapped, concatenated, or truncated).
        let mut bytes = gz(&vec![b'a'; 100_000]);
        let n = bytes.len();
        bytes[n - 4..].copy_from_slice(&1u32.to_le_bytes());
        let (_d, p) = tmp(&bytes);
        assert!(matches!(
            choose(crate::cli::InflateMode::Auto, &p, 64 * 1024 * 1024),
            Strategy::Stream
        ));
    }

    #[test]
    fn explicit_modes_override_the_heuristic() {
        let (_d, p) = tmp(&gz(&vec![b'a'; 5000]));
        assert!(matches!(choose(crate::cli::InflateMode::Stream, &p, u64::MAX), Strategy::Stream));
        assert!(matches!(choose(crate::cli::InflateMode::Buffer, &p, 0), Strategy::Buffer { .. }));
    }

    #[test]
    fn buffered_inflate_grows_when_the_hint_was_too_small() {
        // ISIZE lies (concatenated stream); inflate must still succeed. The
        // body must exceed the 65_536-byte capacity floor in
        // `inflate_buffered`, or the grow/retry loop never actually runs and
        // this test would pass without exercising it.
        let (_d, p) = tmp(&gz(&vec![b'z'; 200_000]));
        let out = inflate_buffered(&p, 16, 1 << 20).unwrap();
        assert_eq!(out.len(), 200_000);
    }

    // --- Fix round 1: --inflate-budget is a hard ceiling in every mode ---

    #[test]
    fn buffer_mode_capacity_and_ceiling_never_exceed_the_budget() {
        let (_d, p) = tmp(&gz(&vec![b'a'; 500_000]));
        let budget = 10_000u64;
        match choose(InflateMode::Buffer, &p, budget) {
            Strategy::Buffer { capacity, ceiling } => {
                let bound = budget.max(1 << 16);
                assert!(capacity as u64 <= bound, "capacity {capacity} exceeds budget-derived bound {bound}");
                assert!(ceiling as u64 <= bound, "ceiling {ceiling} exceeds budget-derived bound {bound}");
            }
            Strategy::Stream => {} // also an acceptable outcome per the compressed-size guard
        }
    }

    #[test]
    fn a_compressed_file_larger_than_the_budget_selects_stream() {
        // Explicit `buffer` mode never runs the `auto` heuristic's
        // hint-vs-compressed "implausible" check, so this is the case that
        // isolates the compressed-file-size guard itself: a large,
        // incompressible body keeps the on-disk gzip well above the budget's
        // effective ceiling even though the mode is forced.
        let (_d, p) = tmp(&gz(&incompressible(200_000)));
        assert!(matches!(choose(InflateMode::Buffer, &p, 1024), Strategy::Stream));
    }

    #[test]
    fn inflate_budget_zero_still_means_never_buffer_in_auto() {
        let (_d, p) = tmp(&gz(&vec![b'a'; 5000]));
        assert!(matches!(choose(InflateMode::Auto, &p, 0), Strategy::Stream));
    }
}
