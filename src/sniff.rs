//! Content-based format detection. There is no `-z` flag by design.

/// One tar block: enough to reach the `ustar` magic at offset 257.
pub const SNIFF_LEN: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Gzip,
    Zstd,
    Xz,
    Bzip2,
    Lz4,
    /// Unix `compress`, `.Z`.
    CompressZ,
    Zip,
    Tar,
    Plain,
}

const LZ4_FRAME: [u8; 4] = [0x04, 0x22, 0x4D, 0x18];
/// `lz4 -l`, the format the Linux kernel uses. lz4_flex reads it too.
const LZ4_LEGACY: [u8; 4] = [0x02, 0x21, 0x4C, 0x18];
/// pi and sqrt(pi), in BCD: bzip2's block and end-of-stream magics.
const BZ_BLOCK: [u8; 6] = [0x31, 0x41, 0x59, 0x26, 0x53, 0x59];
const BZ_END: [u8; 6] = [0x17, 0x72, 0x45, 0x38, 0x50, 0x90];

fn is_skippable(b: &[u8]) -> bool {
    b.len() >= 4 && (0x50..=0x5F).contains(&b[0]) && b[1..4] == [0x2A, 0x4D, 0x18]
}

/// How many bytes of leading skippable frames — each a 4-byte magic, a 4-byte
/// little-endian length, then that many bytes — `head` starts with. `None` if
/// one runs past the end of `head`.
pub fn skippable_prefix(head: &[u8]) -> Option<usize> {
    let mut at = 0usize;
    while is_skippable(&head[at..]) {
        let len = head.get(at + 4..at + 8)?;
        let len = u32::from_le_bytes(len.try_into().unwrap()) as usize;
        at = at.checked_add(8 + len).filter(|&next| next <= head.len())?;
    }
    Some(at)
}

/// The format of the first real frame after the skippable ones.
fn after_skippable(head: &[u8]) -> Format {
    let Some(at) = skippable_prefix(head) else { return Format::Zstd };
    match head.get(at..at + 4) {
        Some(m) if m == LZ4_FRAME || m == LZ4_LEGACY => Format::Lz4,
        _ => Format::Zstd,
    }
}

/// Classify a buffer by its leading bytes. Order matters: the compressed
/// formats are checked first, because a compressed stream can contain any byte
/// sequence at all — including a convincing `ustar` at offset 257.
pub fn sniff(head: &[u8]) -> Format {
    if head.len() >= 2 && head[0] == 0x1f && head[1] == 0x8b {
        return Format::Gzip;
    }
    // gzip's neighbour: same first byte, and `gzip -d` reads both.
    if head.len() >= 2 && head[0] == 0x1f && head[1] == 0x9d {
        return Format::CompressZ;
    }
    // A local file header, or the end-of-central-directory record that is all
    // an empty zip holds. "PK\x07\x08" is the marker a split archive's first
    // segment starts with, and a single-segment one is an ordinary zip after it.
    if head.len() >= 4
        && head[..2] == *b"PK"
        && matches!(head[2..4], [3, 4] | [5, 6] | [7, 8])
    {
        return Format::Zip;
    }
    if head.len() >= 4 && head[..4] == [0x28, 0xB5, 0x2F, 0xFD] {
        return Format::Zstd;
    }
    if head.len() >= 4 && (head[..4] == LZ4_FRAME || head[..4] == LZ4_LEGACY) {
        return Format::Lz4;
    }
    // A skippable frame (magic 0x184D2A50..=0x184D2A5F), which pzstd and
    // others put before the data frames. zstd and lz4 share the range, so the
    // frame is stepped over and the format read from what follows it. Taken
    // for plain text, the archive's compressed bytes were searched as-is for
    // an exit 1 about data never read, so a skippable frame whose end is past
    // the sniff window still counts as compressed: as zstd, the format that
    // writes them in practice.
    if is_skippable(head) {
        return after_skippable(head);
    }
    if head.len() >= 6 && head[..6] == [0xFD, b'7', b'z', b'X', b'Z', 0x00] {
        return Format::Xz;
    }
    // "BZh", a block size '1'..='9', then the magic of the first block or, for
    // an empty stream, of the end of stream. Four printable bytes alone could
    // start a log line; the six binary ones after them cannot.
    if head.len() >= 10
        && head[..3] == *b"BZh"
        && (b'1'..=b'9').contains(&head[3])
        && (head[4..10] == BZ_BLOCK || head[4..10] == BZ_END)
    {
        return Format::Bzip2;
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

    /// pzstd and other tools start with a skippable frame (magic
    /// 0x184D2A50..=0x184D2A5F, little-endian) before any data frame. Taken
    /// for plain text, the archive's compressed bytes were "searched" and the
    /// run exited 1: clean logs, from data never read.
    #[test]
    fn a_skippable_frame_is_zstd() {
        for low in 0x50..=0x5F {
            assert_eq!(sniff(&[low, 0x2A, 0x4D, 0x18, 4, 0, 0, 0]), Format::Zstd, "magic {low:#x}");
        }
        assert_eq!(sniff(&[0x60, 0x2A, 0x4D, 0x18, 4, 0, 0, 0]), Format::Plain, "just past the range");
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
    fn detects_xz_from_magic() {
        assert_eq!(sniff(&[0xFD, b'7', b'z', b'X', b'Z', 0x00, 0x00]), Format::Xz);
    }

    #[test]
    fn xz_magic_needs_all_six_bytes() {
        assert_eq!(sniff(&[0xFD, b'7', b'z', b'X', b'Z']), Format::Plain);
    }

    #[test]
    fn xz_wins_over_a_coincidental_ustar() {
        let mut b = vec![0u8; 512];
        b[..6].copy_from_slice(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]);
        b[257..262].copy_from_slice(b"ustar");
        assert_eq!(sniff(&b), Format::Xz);
    }

    fn bz2_head(level: u8) -> Vec<u8> {
        let mut b = b"BZh".to_vec();
        b.push(level);
        b.extend_from_slice(&BZ_BLOCK);
        b
    }

    #[test]
    fn detects_bzip2_from_magic() {
        for level in b'1'..=b'9' {
            assert_eq!(sniff(&bz2_head(level)), Format::Bzip2, "BZh{}", level as char);
        }
    }

    /// `bzip2 < /dev/null`: no block, straight to the end-of-stream magic.
    #[test]
    fn an_empty_bzip2_stream_is_bzip2() {
        let mut b = b"BZh9".to_vec();
        b.extend_from_slice(&BZ_END);
        assert_eq!(sniff(&b), Format::Bzip2);
    }

    /// "BZh" is three printable letters. A log line starting with "BZh9" must
    /// stay text.
    #[test]
    fn bzip2_needs_more_than_its_printable_prefix() {
        assert_eq!(sniff(b"BZh9 is not a block size\n"), Format::Plain);
        assert_eq!(sniff(b"BZh0"), Format::Plain);
        let mut zero = b"BZh0".to_vec();
        zero.extend_from_slice(&BZ_BLOCK);
        assert_eq!(sniff(&zero), Format::Plain, "level 0 does not exist");
    }

    #[test]
    fn bzip2_wins_over_a_coincidental_ustar() {
        let mut b = vec![0u8; 512];
        b[..10].copy_from_slice(&bz2_head(b'9'));
        b[257..262].copy_from_slice(b"ustar");
        assert_eq!(sniff(&b), Format::Bzip2);
    }

    #[test]
    fn detects_lz4_frame_and_legacy_magics() {
        assert_eq!(sniff(&[0x04, 0x22, 0x4D, 0x18, 0x64]), Format::Lz4);
        assert_eq!(sniff(&[0x02, 0x21, 0x4C, 0x18, 0x00]), Format::Lz4);
        assert_eq!(sniff(&[0x04, 0x22, 0x4D]), Format::Plain, "needs all four bytes");
    }

    #[test]
    fn lz4_wins_over_a_coincidental_ustar() {
        let mut b = vec![0u8; 512];
        b[..4].copy_from_slice(&LZ4_FRAME);
        b[257..262].copy_from_slice(b"ustar");
        assert_eq!(sniff(&b), Format::Lz4);
    }

    /// lz4 shares zstd's skippable-frame range, so the frame is stepped over
    /// and the format read from the next magic.
    #[test]
    fn a_skippable_frame_before_lz4_is_lz4() {
        let mut b = vec![0x50, 0x2A, 0x4D, 0x18, 4, 0, 0, 0, 1, 2, 3, 4];
        b.extend_from_slice(&LZ4_FRAME);
        assert_eq!(sniff(&b), Format::Lz4);
    }

    #[test]
    fn a_skippable_frame_before_zstd_is_still_zstd() {
        let mut b = vec![0x5F, 0x2A, 0x4D, 0x18, 0, 0, 0, 0];
        b.extend_from_slice(&[0x50, 0x2A, 0x4D, 0x18, 2, 0, 0, 0, 9, 9]);
        b.extend_from_slice(&[0x28, 0xB5, 0x2F, 0xFD]);
        assert_eq!(sniff(&b), Format::Zstd, "two skippable frames, then zstd");
    }

    /// A skippable frame longer than the sniff window hides what follows. It
    /// is compressed either way, and must never fall through to Plain.
    #[test]
    fn a_skippable_frame_past_the_window_is_zstd_not_plain() {
        let mut b = vec![0x50, 0x2A, 0x4D, 0x18, 0xFF, 0xFF, 0xFF, 0xFF];
        b.resize(SNIFF_LEN, 0);
        assert_eq!(sniff(&b), Format::Zstd);
        assert_eq!(sniff(&[0x50, 0x2A, 0x4D, 0x18, 4]), Format::Zstd, "truncated length");
    }

    #[test]
    fn detects_compress_z_from_magic() {
        assert_eq!(sniff(&[0x1f, 0x9d, 0x90]), Format::CompressZ);
        assert_eq!(sniff(&[0x1f]), Format::Plain);
    }

    #[test]
    fn detects_zip_from_its_record_signatures() {
        assert_eq!(sniff(b"PK\x03\x04\x14\x00"), Format::Zip, "local file header");
        assert_eq!(sniff(b"PK\x05\x06\x00\x00"), Format::Zip, "empty zip");
        assert_eq!(sniff(b"PK\x07\x08PK\x03\x04"), Format::Zip, "split marker");
        assert_eq!(sniff(b"PK is a log line\n"), Format::Plain);
        assert_eq!(sniff(b"PK\x01\x02"), Format::Plain, "a central record does not start a file");
    }

    #[test]
    fn zip_wins_over_a_coincidental_ustar() {
        let mut b = vec![0u8; 512];
        b[..4].copy_from_slice(b"PK\x03\x04");
        b[257..262].copy_from_slice(b"ustar");
        assert_eq!(sniff(&b), Format::Zip);
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
