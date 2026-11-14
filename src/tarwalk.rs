//! The tar reader: headers in, members with content out.
//!
//! trg's own, where it was the `tar` crate's, because what a search needs
//! from a tar is not what an extractor needs:
//!
//! - **Every byte of content is a member.** An extractor skips what it cannot
//!   create; a search that skips an entry says 1 about bytes it never read.
//!   So an entry is searched whatever its typeflag, unless it is one of the
//!   headers that describe the next entry or it has no data at all.
//! - **Nothing the archive declares is read into memory unseen.** A long-name
//!   or pax header is a size field and that many bytes; one declaring a
//!   gigabyte was a gigabyte of memory. They are capped at [`EXT_MAX`].
//! - **Sparse members are read as the file they were**, holes and all, from
//!   any of the four maps GNU tar and bsdtar have written.
//! - **The end is where the archive says it is.** Only zero blocks at a
//!   header boundary are the end-of-archive mark, so a tar cut after a member
//!   whose data ends in zeros is still cut.

use std::io::{self, Read};

use crate::sniff::{is_tar_header, octal};

const BLOCK: usize = 512;

/// The most a long-name, long-link or pax header may hold, as in libarchive.
/// PATH_MAX is 4096; a megabyte is room for any real record.
pub(crate) const EXT_MAX: u64 = 1 << 20;

/// The most segments a sparse map may have: 16 MiB of map at the worst. The
/// old GNU format chains its map through any number of 512-byte blocks, and
/// would otherwise be memory without limit again.
const SEGMENTS_MAX: usize = 1 << 20;

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn cut(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, format!("the archive ends inside {what}: cut short"))
}

/// The archive ended at a header boundary with no end-of-archive block.
/// Worded apart from the other errors, which name the entry they are about.
pub(crate) const NO_END: &str = "the tar ends without its end-of-archive blocks: cut short";

/// A size or offset field: octal, or GNU's base-256 for what octal cannot
/// hold. An empty field is zero, as old tars left a directory's.
fn number(field: &[u8]) -> Option<u64> {
    if field[0] & 0x80 != 0 {
        return field.iter().enumerate().try_fold(0u64, |v, (i, &b)| {
            let b = if i == 0 { b & 0x7F } else { b };
            (v >> 56 == 0).then_some((v << 8) | b as u64)
        });
    }
    if field.iter().all(|&b| b == 0 || b == b' ') {
        return Some(0);
    }
    octal(field)
}

fn decimal(text: &[u8]) -> Option<u64> {
    if text.is_empty() {
        return None;
    }
    text.iter().try_fold(0u64, |v, &b| {
        b.is_ascii_digit().then_some(())?;
        v.checked_mul(10)?.checked_add((b - b'0') as u64)
    })
}

fn until_nul(field: &[u8]) -> &[u8] {
    &field[..memchr::memchr(0, field).unwrap_or(field.len())]
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The name in the header itself: `prefix/name` in a ustar header, `name`
/// alone in the others, where the prefix's bytes hold other things.
fn header_name(h: &[u8; BLOCK]) -> String {
    let name = until_nul(&h[..100]);
    let prefix = until_nul(&h[345..500]);
    if &h[257..265] == b"ustar\x0000" && !prefix.is_empty() {
        return format!("{}/{}", text(prefix), text(name));
    }
    text(name)
}

/// What the extension headers before an entry said about it.
#[derive(Default)]
struct Ext {
    /// GNU 'L'.
    long_name: Option<String>,
    path: Option<String>,
    size: Option<u64>,
    sparse_name: Option<String>,
    /// `GNU.sparse.major` is 1: the map leads the data.
    map_in_data: bool,
    /// `GNU.sparse.map`, or `GNU.sparse.offset` and `numbytes` in turn.
    map: Option<Vec<u64>>,
    realsize: Option<u64>,
    any: bool,
}

impl Ext {
    fn pax(&mut self, mut data: &[u8]) -> io::Result<()> {
        let malformed = || bad("a malformed pax header");
        while !data.is_empty() {
            let space = memchr::memchr(b' ', data).ok_or_else(malformed)?;
            let len = decimal(&data[..space]).ok_or_else(malformed)? as usize;
            if len <= space + 1 || len > data.len() || data[len - 1] != b'\n' {
                return Err(malformed());
            }
            let record = &data[space + 1..len - 1];
            data = &data[len..];
            let eq = memchr::memchr(b'=', record).ok_or_else(malformed)?;
            let (key, value) = (&record[..eq], &record[eq + 1..]);
            let num = || decimal(value).ok_or_else(|| bad(format!("pax {}: not a number", text(key))));
            match key {
                b"path" => self.path = Some(text(value)),
                b"size" => self.size = Some(num()?),
                b"GNU.sparse.name" => self.sparse_name = Some(text(value)),
                b"GNU.sparse.major" => self.map_in_data = num()? >= 1,
                b"GNU.sparse.realsize" | b"GNU.sparse.size" => self.realsize = Some(num()?),
                b"GNU.sparse.offset" | b"GNU.sparse.numbytes" => {
                    let map = self.map.get_or_insert_default();
                    // Offsets at even places, lengths at odd ones.
                    if !map.len().is_multiple_of(2) != (key == b"GNU.sparse.numbytes") {
                        return Err(bad("a sparse map out of order"));
                    }
                    map.push(num()?);
                }
                b"GNU.sparse.map" => {
                    let parsed: Option<Vec<u64>> = value.split(|&b| b == b',').map(decimal).collect();
                    self.map = Some(parsed.ok_or_else(|| bad("a sparse map that is not numbers"))?);
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// A sparse member: where each stretch of stored data belongs, and how long
/// the file really is.
struct Sparse {
    /// (offset, length), in order and not overlapping.
    segments: Vec<(u64, u64)>,
    next: usize,
    at: u64,
    realsize: u64,
}

impl Sparse {
    /// `stored` is how much data the archive holds for the member, which the
    /// map must account for to the byte: with a map that does not, some of
    /// the data would be read as the wrong part of the file, or not at all.
    fn new(map: &[u64], realsize: Option<u64>, stored: u64) -> io::Result<Sparse> {
        let wrong = || bad("a sparse map that does not match the member's data");
        if !map.len().is_multiple_of(2) {
            return Err(wrong());
        }
        let segments: Vec<(u64, u64)> = map.chunks(2).map(|p| (p[0], p[1])).collect();
        let (mut end, mut total) = (0u64, 0u64);
        for &(offset, len) in &segments {
            if offset < end {
                return Err(wrong());
            }
            end = offset.checked_add(len).ok_or_else(wrong)?;
            total = total.checked_add(len).ok_or_else(wrong)?;
        }
        let realsize = realsize.unwrap_or(end);
        if total != stored || realsize < end {
            return Err(wrong());
        }
        Ok(Sparse { segments, next: 0, at: 0, realsize })
    }
}

/// A tar, read one member at a time.
pub(crate) struct Tar<R> {
    inner: R,
    /// A block read ahead and given back, served before `inner`.
    back: [u8; BLOCK],
    back_at: usize,
    back_len: usize,
    /// What is left of the current member on the archive: data, then the
    /// padding to the next block.
    data: u64,
    padding: u64,
    sparse: Option<Sparse>,
    /// A zero block has been read since the last entry.
    end_seen: bool,
    scratch: Vec<u8>,
}

impl<R: Read> Tar<R> {
    pub(crate) fn new(inner: R) -> Self {
        Tar {
            inner,
            back: [0; BLOCK],
            back_at: 0,
            back_len: 0,
            data: 0,
            padding: 0,
            sparse: None,
            end_seen: false,
            scratch: Vec::new(),
        }
    }

    /// Whether the archive's end-of-archive mark has been read, and nothing
    /// but zeros after it.
    pub(crate) fn end_seen(&self) -> bool {
        self.end_seen
    }

    fn read_raw(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.back_at < self.back_len {
            let n = buf.len().min(self.back_len - self.back_at);
            buf[..n].copy_from_slice(&self.back[self.back_at..self.back_at + n]);
            self.back_at += n;
            return Ok(n);
        }
        self.inner.read(buf)
    }

    /// Fill `buf`, or as much of it as there is before the end of the input.
    fn fill(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut n = 0;
        while n < buf.len() {
            match self.read_raw(&mut buf[n..]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(n)
    }

    fn block(&mut self, what: &str) -> io::Result<[u8; BLOCK]> {
        let mut b = [0u8; BLOCK];
        if self.fill(&mut b)? < BLOCK {
            return Err(cut(what));
        }
        Ok(b)
    }

    /// Read past `n` bytes.
    fn skip(&mut self, mut n: u64, what: &str) -> io::Result<()> {
        if n == 0 {
            return Ok(());
        }
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.resize(64 * 1024, 0);
        let mut res = Ok(());
        while n > 0 {
            let want = n.min(scratch.len() as u64) as usize;
            match self.read_raw(&mut scratch[..want]) {
                Ok(0) => {
                    res = Err(cut(what));
                    break;
                }
                Ok(k) => n -= k as u64,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    res = Err(e);
                    break;
                }
            }
        }
        self.scratch = scratch;
        res
    }

    /// The data of an extension header, whole: `size` is within [`EXT_MAX`].
    fn extension(&mut self, size: u64) -> io::Result<Vec<u8>> {
        let mut data = vec![0u8; size as usize];
        if self.fill(&mut data)? < data.len() {
            return Err(cut("an extension header"));
        }
        self.skip(size.next_multiple_of(BLOCK as u64) - size, "an extension header")?;
        Ok(data)
    }

    /// Whether data follows the header just read, for an entry of a type
    /// that has none but states a size.
    ///
    /// Old tars gave a hard link, and some a directory, the size of the file
    /// it names, with no data: GNU tar, Python and Go all read them so. pax
    /// allows a hard link its data. Which it is shows in the next block, as
    /// libarchive reads it too: a header or the archive's end means there was
    /// no data. Anything else is data, and is searched, since the reason to
    /// tell at all is that data must not go unread.
    fn data_follows(&mut self) -> io::Result<bool> {
        let mut next = [0u8; BLOCK];
        let n = self.fill(&mut next)?;
        let header_or_end = n == 0 || (n == BLOCK && (next == [0u8; BLOCK] || is_tar_header(&next)));
        self.back = next;
        self.back_at = 0;
        self.back_len = n;
        Ok(!header_or_end)
    }

    /// The old GNU sparse map: four entries in the header, and 21 more in
    /// each of the extension blocks that follow it while one is flagged.
    fn old_gnu_map(&mut self, h: &[u8; BLOCK]) -> io::Result<Vec<u64>> {
        fn entries(fields: &[u8], map: &mut Vec<u64>) -> io::Result<()> {
            for e in fields.as_chunks::<24>().0 {
                // GNU tar ends the map at the first entry left empty.
                if e[12] == 0 {
                    break;
                }
                let n = |f: &[u8]| number(f).ok_or_else(|| bad("a sparse map that is not numbers"));
                map.push(n(&e[..12])?);
                map.push(n(&e[12..])?);
            }
            Ok(())
        }
        let mut map = Vec::new();
        entries(&h[386..482], &mut map)?;
        let mut more = h[482] != 0;
        while more {
            let x = self.block("a sparse map")?;
            entries(&x[..504], &mut map)?;
            more = x[504] != 0;
            if map.len() > 2 * SEGMENTS_MAX {
                return Err(bad(format!("a sparse map of over {SEGMENTS_MAX} segments: not read")));
            }
        }
        Ok(map)
    }

    /// The pax 1.0 sparse map, which leads the member's data: a count, then
    /// that many offsets and lengths, each a decimal number and a newline,
    /// filled to a whole block. Takes what it reads off `stored`.
    fn map_in_data(&mut self, stored: &mut u64) -> io::Result<Vec<u64>> {
        let wrong = || bad("a sparse map that is not numbers");
        let mut map = Vec::new();
        let mut count: Option<u64> = None;
        let mut number: Option<u64> = None;
        loop {
            if *stored < BLOCK as u64 {
                return Err(bad("a sparse map longer than its member"));
            }
            let b = self.block("a sparse map")?;
            *stored -= BLOCK as u64;
            for &c in &b {
                match c {
                    b'0'..=b'9' => {
                        let v = number.unwrap_or(0).checked_mul(10).and_then(|v| v.checked_add((c - b'0') as u64));
                        number = Some(v.ok_or_else(wrong)?);
                    }
                    b'\n' => {
                        let v = number.take().ok_or_else(wrong)?;
                        match count {
                            None if v > SEGMENTS_MAX as u64 => {
                                return Err(bad(format!("a sparse map of over {SEGMENTS_MAX} segments: not read")));
                            }
                            None => count = Some(v),
                            Some(_) => map.push(v),
                        }
                        if count == Some(map.len() as u64 / 2) && map.len().is_multiple_of(2) {
                            return Ok(map);
                        }
                    }
                    _ => return Err(wrong()),
                }
            }
        }
    }

    /// Move to the next member that has content, and return its name.
    /// `None` at the end of the archive, which is the end of the input after
    /// at least one zero block: [`NO_END`] is the error without one.
    ///
    /// The rest of the member before it is read past, so a decoder under the
    /// tar sees every byte and gets to its checksum.
    pub(crate) fn next_member(&mut self) -> io::Result<Option<String>> {
        let rest = std::mem::take(&mut self.data) + std::mem::take(&mut self.padding);
        self.sparse = None;
        self.skip(rest, "a member")?;

        let mut ext = Ext::default();
        loop {
            let mut h = [0u8; BLOCK];
            let n = self.fill(&mut h)?;
            if h[..n].iter().all(|&b| b == 0) {
                // Zero blocks end an archive, and another may follow them,
                // as in `cat a.tar b.tar`. A last short run of zeros is what
                // padding decodes to under `.Z`, where bsdtar fills its
                // output to 10 KiB with bytes that are codes to LZW.
                if n == BLOCK {
                    self.end_seen = true;
                    continue;
                }
                if ext.any {
                    return Err(cut("an extension header"));
                }
                if !self.end_seen {
                    return Err(bad(NO_END));
                }
                return Ok(None);
            }
            if n < BLOCK {
                return Err(cut("a header"));
            }
            if !is_tar_header(&h) {
                return Err(bad("a block that is neither a tar header nor zeros"));
            }
            self.end_seen = false;

            let flag = h[156];
            let size = number(&h[124..136]).ok_or_else(|| bad("a header whose size is not a number"))?;
            if matches!(flag, b'L' | b'K' | b'x' | b'X' | b'g') {
                // Before any of it is read: the tar crate read these whole,
                // and a header declaring a gigabyte was a gigabyte of memory.
                if size > EXT_MAX {
                    let kind = match flag {
                        b'L' => "long-name",
                        b'K' => "long-link",
                        _ => "pax",
                    };
                    return Err(bad(format!(
                        "a {kind} header of {size} bytes, over the limit of {EXT_MAX}: not read"
                    )));
                }
                let data = self.extension(size)?;
                match flag {
                    b'L' => ext.long_name = Some(text(until_nul(&data))),
                    b'x' | b'X' => ext.pax(&data)?,
                    // A link's target and a global header name no member.
                    _ => {}
                }
                ext.any |= flag != b'g';
                continue;
            }

            let mut stored = ext.size.unwrap_or(size);
            let name = ext
                .sparse_name
                .take()
                .or(ext.long_name.take())
                .or(ext.path.take())
                .unwrap_or_else(|| header_name(&h));

            let map = if flag == b'S' {
                Some(self.old_gnu_map(&h)?)
            } else if ext.map_in_data {
                Some(self.map_in_data(&mut stored)?)
            } else {
                ext.map.take()
            };
            let realsize = if flag == b'S' { number(&h[483..495]) } else { ext.realsize };
            let sparse = match map {
                Some(map) => Some(Sparse::new(&map, realsize, stored).map_err(|e| bad(format!("{name}: {e}")))?),
                None => None,
            };
            ext = Ext::default();

            // Link, symlink, character and block device, directory, fifo:
            // entries that are a header and nothing more.
            if matches!(flag, b'1'..=b'6') && stored > 0 && sparse.is_none() && !self.data_follows()? {
                continue;
            }
            // And before there was a typeflag for one, a directory was a
            // name ending in a slash.
            let v7_dir = matches!(flag, 0 | b'0') && name.ends_with('/');
            if stored == 0 && (matches!(flag, b'1'..=b'6') || v7_dir) {
                continue;
            }

            self.data = stored;
            self.padding = stored.next_multiple_of(BLOCK as u64) - stored;
            self.sparse = sparse;
            return Ok(Some(name));
        }
    }

    /// The current member's content. For a sparse member that is the file
    /// it was, its holes read as zeros.
    pub(crate) fn content(&mut self) -> Content<'_, R> {
        Content(self)
    }

    /// The archive ending inside a member reads as the member ending, and
    /// is reported by the [`Tar::next_member`] after it.
    fn read_stored(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let want = (buf.len() as u64).min(self.data) as usize;
        if want == 0 {
            return Ok(0);
        }
        let n = self.read_raw(&mut buf[..want])?;
        self.data -= n as u64;
        Ok(n)
    }

    fn read_sparse(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let s = self.sparse.as_mut().expect("called for a sparse member");
        if buf.is_empty() || s.at >= s.realsize {
            return Ok(0);
        }
        while s.segments.get(s.next).is_some_and(|&(offset, len)| s.at >= offset + len) {
            s.next += 1;
        }
        // To the end of the segment `at` is in, or of the hole before the
        // next one, or of the hole the file ends in.
        let (until, stored) = match s.segments.get(s.next) {
            Some(&(offset, len)) if s.at >= offset => (offset + len, true),
            Some(&(offset, _)) => (offset, false),
            None => (s.realsize, false),
        };
        let want = (until - s.at).min(buf.len() as u64) as usize;
        let n = if stored {
            self.read_stored(&mut buf[..want])?
        } else {
            buf[..want].fill(0);
            want
        };
        self.sparse.as_mut().expect("checked above").at += n as u64;
        Ok(n)
    }
}

/// See [`Tar::content`].
pub(crate) struct Content<'a, R>(&'a mut Tar<R>);

impl<R: Read> Read for Content<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.0.sparse {
            None => self.0.read_stored(buf),
            Some(_) => self.0.read_sparse(buf),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(name: &str, flag: u8, size: u64) -> Vec<u8> {
        let mut h = vec![0u8; BLOCK];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        h[156] = flag;
        seal(&mut h);
        h
    }

    fn seal(h: &mut [u8]) {
        h[148..156].fill(b' ');
        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    }

    fn blocks(data: &[u8]) -> Vec<u8> {
        let mut v = data.to_vec();
        v.resize(data.len().next_multiple_of(BLOCK), 0);
        v
    }

    fn members(bytes: &[u8]) -> io::Result<Vec<(String, Vec<u8>)>> {
        let mut tar = Tar::new(bytes);
        let mut out = Vec::new();
        while let Some(name) = tar.next_member()? {
            let mut data = Vec::new();
            tar.content().read_to_end(&mut data)?;
            out.push((name, data));
        }
        Ok(out)
    }

    #[test]
    fn numbers_are_octal_or_base_256() {
        assert_eq!(number(b"00000000017\0"), Some(15));
        assert_eq!(number(b"         17 "), Some(15));
        assert_eq!(number(&[0; 12]), Some(0), "an empty field");
        assert_eq!(number(b"0000000001x\0"), None);
        let mut big = [0u8; 12];
        big[0] = 0x80;
        big[7] = 2;
        assert_eq!(number(&big), Some(2 << 32), "8 GiB, past what octal holds");
        big[1] = 1;
        assert_eq!(number(&big), None, "past 64 bits");
    }

    /// A member left unread, or half read, is read past.
    #[test]
    fn a_member_not_read_does_not_shift_the_next() {
        let mut t = header("a", b'0', 600);
        t.extend(blocks(&[b'a'; 600]));
        t.extend(header("b", b'0', 3));
        t.extend(blocks(b"bbb"));
        t.extend([0u8; 1024]);
        let mut tar = Tar::new(&t[..]);
        assert_eq!(tar.next_member().unwrap().as_deref(), Some("a"));
        let mut some = [0u8; 100];
        tar.content().read_exact(&mut some).unwrap();
        assert_eq!(tar.next_member().unwrap().as_deref(), Some("b"));
        let mut data = Vec::new();
        tar.content().read_to_end(&mut data).unwrap();
        assert_eq!(data, b"bbb");
        assert_eq!(tar.next_member().unwrap(), None);
        assert!(tar.end_seen());
    }

    #[test]
    fn a_sparse_member_reads_as_the_file_it_was() {
        let mut h = header("s", b'S', 5);
        for (i, (offset, len)) in [(3u64, 2u64), (10, 3), (20, 0)].iter().enumerate() {
            let at = 386 + i * 24;
            h[at..at + 12].copy_from_slice(format!("{offset:011o}\0").as_bytes());
            h[at + 12..at + 24].copy_from_slice(format!("{len:011o}\0").as_bytes());
        }
        h[483..495].copy_from_slice(format!("{:011o}\0", 20).as_bytes());
        seal(&mut h);
        h.extend(blocks(b"abcde"));
        h.extend(header("after", b'0', 1));
        h.extend(blocks(b"z"));
        h.extend([0u8; 512]);
        let got = members(&h).unwrap();
        assert_eq!(got[0].1, b"\0\0\0ab\0\0\0\0\0cde\0\0\0\0\0\0\0");
        assert_eq!(got[1], ("after".to_string(), b"z".to_vec()));
    }

    #[test]
    fn the_end_is_zero_blocks_at_a_header_boundary() {
        let mut t = header("a", b'0', 512);
        t.extend([0u8; 512]);
        // The member's own zeros are not the mark.
        assert_eq!(members(&t).unwrap_err().to_string(), NO_END);
        t.extend([0u8; 512]);
        assert_eq!(members(&t).unwrap().len(), 1);
        t.extend([0u8; 100]);
        assert_eq!(members(&t).unwrap().len(), 1, "padding after the mark");
        t.extend(b"x");
        assert!(members(&t).is_err(), "and not anything else");
    }

    #[test]
    fn an_extension_header_over_the_limit_is_not_read() {
        let mut t = header("././@LongLink", b'L', EXT_MAX + 1);
        t.extend(vec![b'a'; 4096]);
        let e = members(&t).unwrap_err().to_string();
        assert!(e.contains("long-name header of 1048577 bytes"), "{e}");
    }
}
