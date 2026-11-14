//! Content-based format detection. There is no `-z` flag by design.

/// One tar block: a whole header, checksum and all.
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
    /// A format trg has no reader for, named with its article ("an lzip").
    /// Searched as the bytes it is, a compressed file is a 1 about content
    /// nothing read; so it is an error instead.
    Unreadable(&'static str),
    Plain,
}

impl Format {
    /// Why a file of a format trg cannot read was not searched.
    pub fn unreadable(self) -> Option<String> {
        match self {
            Format::Unreadable(kind) => Some(format!("{kind} file, which trg cannot read: not searched")),
            _ => None,
        }
    }
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

/// A tar number field: octal digits, after spaces and before a NUL or a
/// space. `None` if it holds no digit, or anything else.
pub(crate) fn octal(field: &[u8]) -> Option<u64> {
    let start = field.iter().position(|&b| b != b' ')?;
    let field = &field[start..];
    let digits = field.iter().position(|b| !(b'0'..=b'7').contains(b)).unwrap_or(field.len());
    if digits == 0 || digits > 21 || field[digits..].iter().any(|&b| b != 0 && b != b' ') {
        return None;
    }
    Some(field[..digits].iter().fold(0u64, |v, &b| (v << 3) | (b - b'0') as u64))
}

/// Whether `block` starts with a tar header: 512 bytes whose checksum field
/// holds the sum of the rest, taken as unsigned bytes or, as some old tars
/// wrote it, signed ones.
///
/// The checksum, and not the `ustar` magic at offset 257. A v7 tar has no
/// magic, and was searched as a plain file with its compressed members
/// unopened; and a log can have the magic, whoever sends its first request
/// line choosing the bytes, and was then read as a broken tar and not
/// searched.
pub fn is_tar_header(block: &[u8]) -> bool {
    let Some(block) = block.get(..512) else { return false };
    let Some(stored) = octal(&block[148..156]) else { return false };
    let (mut unsigned, mut signed) = (0u64, 0i64);
    for (i, &b) in block.iter().enumerate() {
        let b = if (148..156).contains(&i) { b' ' } else { b };
        unsigned += b as u64;
        signed += b as i8 as i64;
    }
    stored == unsigned || i64::try_from(stored) == Ok(signed)
}

fn all(bytes: Option<&[u8]>, f: fn(&u8) -> bool) -> bool {
    bytes.is_some_and(|b| b.iter().all(f))
}

/// A binary cpio header: thirteen 16-bit fields in either byte order, then
/// the name. The two bytes of its magic are too few to go on (`q` and a
/// Latin-1 letter), so the file type and the name must make sense as well.
fn is_binary_cpio(head: &[u8]) -> bool {
    let field: fn([u8; 2]) -> u16 = match head {
        [0xC7, 0x71, ..] => u16::from_le_bytes,
        [0x71, 0xC7, ..] => u16::from_be_bytes,
        _ => return false,
    };
    let at = |i: usize| head.get(i..i + 2).map(|b| field([b[0], b[1]]));
    let (Some(mode), Some(namesize)) = (at(6), at(20)) else { return false };
    let namesize = namesize as usize;
    let Some(name) = head.get(26..26 + namesize) else { return false };
    matches!(mode >> 12, 0o01 | 0o02 | 0o04 | 0o06 | 0o10 | 0o12 | 0o14)
        && name.last() == Some(&0)
        && !name[..namesize - 1].contains(&0)
}

/// An lzma-alone file (`xz --format=lzma`, `.lzma`), which has no magic:
/// the properties byte every encoder writes, a dictionary size that is 2^n
/// or 2^n + 2^(n-1), a length that is unknown or plausible, and the zero
/// byte every range-coded stream starts with.
fn is_lzma_alone(head: &[u8]) -> bool {
    let Some(h) = head.get(..14) else { return false };
    let dict = u32::from_le_bytes(h[1..5].try_into().unwrap());
    let size = u64::from_le_bytes(h[5..13].try_into().unwrap());
    let top = 1u32 << (31 - dict.leading_zeros().min(31));
    h[0] == 0x5D
        && dict >= 1 << 16
        && (dict == top || dict == (top | (top >> 1)))
        && (size == u64::MAX || size < 1 << 40)
        && h[13] == 0
}

/// The formats trg knows by sight and cannot read.
fn unreadable(head: &[u8]) -> Option<&'static str> {
    // "LZIP" and a version, of which there are two.
    if head.starts_with(b"LZIP") && matches!(head.get(4), Some(0 | 1)) {
        return Some("an lzip");
    }
    if head.starts_with(b"7z\xBC\xAF\x27\x1C") {
        return Some("a 7z");
    }
    if head.starts_with(b"Rar!\x1a\x07") {
        return Some("a rar");
    }
    if head.starts_with(b"\x89LZO\x00\r\n\x1a\n") {
        return Some("an lzop");
    }
    // The text cpio headers are all digits, 76 octal ones or 110 hex, and a
    // log line can start with the six of the magic.
    if head.starts_with(b"070707") && all(head.get(..76), |b| (b'0'..=b'7').contains(b)) {
        return Some("a cpio");
    }
    if (head.starts_with(b"070701") || head.starts_with(b"070702")) && all(head.get(..110), u8::is_ascii_hexdigit) {
        return Some("a cpio");
    }
    if is_binary_cpio(head) {
        return Some("a cpio");
    }
    if is_lzma_alone(head) {
        return Some("an lzma");
    }
    None
}

/// Classify a buffer by its leading bytes. Order matters: the compressed
/// formats are checked first, because a compressed stream can contain any byte
/// sequence at all.
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
    if is_tar_header(head) {
        return Format::Tar;
    }
    match unreadable(head) {
        Some(kind) => Format::Unreadable(kind),
        None => Format::Plain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_gzip_from_magic() {
        assert_eq!(sniff(&[0x1f, 0x8b, 0x08, 0x00]), Format::Gzip);
    }

    /// A header block with a name, `magic` at 257 and a checksum that holds.
    fn tar_header(magic: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; 512];
        b[..5].copy_from_slice(b"a.log");
        b[257..257 + magic.len()].copy_from_slice(magic);
        b[148..156].fill(b' ');
        let sum: u32 = b.iter().map(|&x| x as u32).sum();
        b[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        b
    }

    #[test]
    fn detects_a_tar_of_any_dialect_by_its_header_checksum() {
        assert_eq!(sniff(&tar_header(b"ustar\x0000")), Format::Tar, "ustar");
        assert_eq!(sniff(&tar_header(b"ustar  \0")), Format::Tar, "GNU");
        assert_eq!(sniff(&tar_header(b"")), Format::Tar, "v7, which has no magic");
    }

    /// Some old tars summed the header as signed bytes, which differs once
    /// a name has a byte over 0x7F.
    #[test]
    fn a_signed_header_checksum_is_a_tar_too() {
        let mut b = tar_header(b"");
        b[0] = 0xE5;
        assert_eq!(sniff(&b), Format::Plain, "the checksum no longer holds");
        b[148..156].fill(b' ');
        let sum: i32 = b.iter().map(|&x| x as i8 as i32).sum();
        b[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        assert_eq!(sniff(&b), Format::Tar);
    }

    /// The magic alone is five bytes anyone can put in a log.
    #[test]
    fn the_ustar_magic_without_a_checksum_is_not_a_tar() {
        let mut b = vec![0u8; 512];
        b[257..262].copy_from_slice(b"ustar");
        assert_eq!(sniff(&b), Format::Plain);
        let mut b = vec![b'a'; 512];
        b[257..262].copy_from_slice(b"ustar");
        assert_eq!(sniff(&b), Format::Plain);
        assert_eq!(sniff(&[0u8; 512]), Format::Plain, "a block of zeros has no checksum");
        assert_eq!(sniff(&tar_header(b"ustar")[..511]), Format::Plain, "short of a block");
    }

    #[test]
    fn formats_without_a_reader_are_named() {
        let kind = |b: &[u8]| sniff(b).unreadable();
        let says = |k: &str| Some(format!("{k} file, which trg cannot read: not searched"));
        assert_eq!(kind(b"LZIP\x01\x0c"), says("an lzip"));
        assert_eq!(kind(b"7z\xBC\xAF\x27\x1C\x00\x04"), says("a 7z"));
        assert_eq!(kind(b"Rar!\x1a\x07\x00"), says("a rar"));
        assert_eq!(kind(b"\x89LZO\x00\r\n\x1a\n\x10"), says("an lzop"));
        assert_eq!(kind(&b"070707".repeat(13)), says("a cpio"));
        assert_eq!(kind(&b"070701".repeat(19)), says("a cpio"));
        assert_eq!(kind(&b"070702".repeat(19)), says("a cpio"));
        // xz --format=lzma: 8 MiB dictionary, unknown length.
        let mut lzma = vec![0x5D, 0, 0, 0x80, 0];
        lzma.extend([0xFF; 8]);
        lzma.extend([0, 0x26, 0x1B]);
        assert_eq!(kind(&lzma), says("an lzma"));
        lzma[13] = 1;
        assert_eq!(kind(&lzma), None, "a range coder starts with a zero");
        lzma[13] = 0;
        lzma[3] = 0x81;
        assert_eq!(kind(&lzma), None, "not a dictionary size");
        assert_eq!(kind(b"plain"), None);
    }

    #[test]
    fn text_that_starts_with_a_printable_magic_is_text() {
        assert_eq!(sniff(b"LZIP is a word\n"), Format::Plain);
        assert_eq!(sniff(b"070707 12:00 GET /\n"), Format::Plain);
        assert_eq!(sniff(&b"070701 ".repeat(30)), Format::Plain);
        assert_eq!(sniff(b"7z, Rar! and friends\n"), Format::Plain);
        assert_eq!(sniff(b"q\xc7 in Latin-1, and a long enough line after it\n"), Format::Plain);
        assert_eq!(sniff(b"]\0\0\x80\0 but not a stream"), Format::Plain);
    }

    #[test]
    fn a_binary_cpio_is_known_in_both_byte_orders() {
        let fields: [u16; 13] = [0o070707, 1, 2, 0o100644, 0, 0, 1, 0, 0, 0, 6, 0, 7];
        for order in [u16::to_le_bytes, u16::to_be_bytes] {
            let mut b: Vec<u8> = fields.iter().flat_map(|&f| order(f)).collect();
            b.extend_from_slice(b"a.log\0data");
            assert_eq!(sniff(&b), Format::Unreadable("a cpio"));
            b[30] = 0;
            assert_eq!(sniff(&b), Format::Plain, "a NUL inside the name");
        }
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
