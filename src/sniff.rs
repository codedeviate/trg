//! Content-based format detection. There is no `-z` flag by design.

/// One tar block: enough to reach the `ustar` magic at offset 257.
pub const SNIFF_LEN: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Gzip,
    Zstd,
    Tar,
    Plain,
}

/// Classify a buffer by its leading bytes. Order matters: the compressed
/// formats are checked first, because a compressed stream can contain any byte
/// sequence at all — including a convincing `ustar` at offset 257.
pub fn sniff(head: &[u8]) -> Format {
    if head.len() >= 2 && head[0] == 0x1f && head[1] == 0x8b {
        return Format::Gzip;
    }
    if head.len() >= 4 && head[..4] == [0x28, 0xB5, 0x2F, 0xFD] {
        return Format::Zstd;
    }
    if head.len() >= 262 && &head[257..262] == b"ustar" {
        return Format::Tar;
    }
    Format::Plain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_gzip_from_magic() {
        assert_eq!(sniff(&[0x1f, 0x8b, 0x08, 0x00]), Format::Gzip);
    }

    #[test]
    fn detects_ustar_tar_at_offset_257() {
        let mut b = vec![0u8; 512];
        b[257..262].copy_from_slice(b"ustar");
        assert_eq!(sniff(&b), Format::Tar);
    }

    #[test]
    fn detects_gnu_tar_variant() {
        let mut b = vec![0u8; 512];
        b[257..265].copy_from_slice(b"ustar  \0");
        assert_eq!(sniff(&b), Format::Tar);
    }

    #[test]
    fn detects_zstd_from_magic() {
        assert_eq!(sniff(&[0x28, 0xB5, 0x2F, 0xFD, 0x00]), Format::Zstd);
    }

    #[test]
    fn zstd_magic_needs_all_four_bytes() {
        // A short prefix must not be mistaken for a zstd frame.
        assert_eq!(sniff(&[0x28, 0xB5, 0x2F]), Format::Plain);
    }

    #[test]
    fn zstd_wins_over_a_coincidental_ustar() {
        let mut b = vec![0u8; 512];
        b[..4].copy_from_slice(&[0x28, 0xB5, 0x2F, 0xFD]);
        b[257..262].copy_from_slice(b"ustar");
        assert_eq!(sniff(&b), Format::Zstd);
    }

    #[test]
    fn plain_text_is_plain() {
        assert_eq!(sniff(b"2026-08-24T00:00:00Z GET /index.php\n"), Format::Plain);
    }

    #[test]
    fn short_input_is_plain_not_panic() {
        assert_eq!(sniff(b""), Format::Plain);
        assert_eq!(sniff(&[0x1f]), Format::Plain);
    }

    #[test]
    fn gzip_wins_over_a_coincidental_ustar() {
        // a gzip stream whose bytes happen to contain "ustar" at 257
        let mut b = vec![0u8; 512];
        b[0] = 0x1f; b[1] = 0x8b;
        b[257..262].copy_from_slice(b"ustar");
        assert_eq!(sniff(&b), Format::Gzip);
    }
}
