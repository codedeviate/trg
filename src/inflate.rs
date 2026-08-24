//! Choosing between streaming and whole-buffer inflate.
//!
//! Whole-buffer (libdeflate) measured ~14x cheaper on CPU but allocates the
//! full decompressed size. Streaming (flate2) stays around 10 MB. `auto` reads
//! the gzip ISIZE trailer as a hint, but correctness comes from the buffered
//! path growing and finally falling back, not from trusting the hint.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use crate::cli::InflateMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    Stream,
    Buffer { capacity: usize },
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

pub fn choose(mode: InflateMode, path: &Path, budget: u64) -> Strategy {
    match mode {
        InflateMode::Stream => Strategy::Stream,
        InflateMode::Buffer => {
            let cap = isize_hint(path).unwrap_or(1 << 20).max(1 << 16);
            Strategy::Buffer { capacity: cap as usize }
        }
        InflateMode::Auto => {
            let Some(hint) = isize_hint(path) else { return Strategy::Stream };
            let compressed = std::fs::metadata(path).map(|m| m.len()).unwrap_or(u64::MAX);

            // gzip never shrinks incompressible data by more than a rounding
            // error, so a hint far below the compressed size means the trailer
            // is not describing this whole stream.
            if hint < compressed / 2 {
                return Strategy::Stream;
            }
            if hint == 0 || hint > budget {
                return Strategy::Stream;
            }
            Strategy::Buffer { capacity: hint as usize }
        }
    }
}

/// Whole-buffer inflate. Doubles and retries when the ISIZE hint was too small
/// (concatenated members), and gives up at 4x the budget so a lying trailer
/// cannot be turned into unbounded allocation by an attacker-supplied archive.
pub fn inflate_buffered(path: &Path, capacity: usize) -> io::Result<Vec<u8>> {
    let raw = std::fs::read(path)?;
    let mut cap = capacity.max(1 << 16);
    let ceiling = cap.saturating_mul(64);

    loop {
        let mut d = libdeflater::Decompressor::new();
        let mut out = vec![0u8; cap];
        match d.gzip_decompress(&raw, &mut out) {
            Ok(n) => {
                out.truncate(n);
                return Ok(out);
            }
            Err(_) if cap < ceiling => {
                cap = cap.saturating_mul(4);
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
        let out = inflate_buffered(&p, 16).unwrap();
        assert_eq!(out.len(), 200_000);
    }
}
