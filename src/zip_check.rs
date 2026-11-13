//! A zip, checked against its own file.
//!
//! A zip is read from the directory at its end, and the directory is the
//! archive's own account of what it holds. Nothing makes that account cover
//! the file: `cat a.zip b.zip` is read as `b.zip` alone, a zip behind a
//! script is not a zip by its first bytes, an end record can count fewer
//! entries than its directory lists, and data can sit between the entries or
//! after the end record with no entry naming it. `unzip` reads the same
//! directory and warns ("extra bytes at beginning or within zipfile"); a
//! search that says nothing and exits 1 has called bytes clean that it never
//! read. So the file is measured here against what its directory lists, and
//! every byte in neither an entry nor the zip's own records is reported.
//! Zeros are padding, as after a tar or a gzip.
//!
//! Positioned reads only: the handle shares its offset with the reader.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;

const LOCAL: &[u8] = b"PK\x03\x04";
const CENTRAL: &[u8] = b"PK\x01\x02";
const END: &[u8] = b"PK\x05\x06";
const END64: &[u8] = b"PK\x06\x06";
const LOCATOR64: &[u8] = b"PK\x06\x07";
/// A data descriptor's optional signature, and the marker a split archive
/// starts with.
const DESCRIPTOR: &[u8] = b"PK\x07\x08";

/// The end record without its comment, and the longest comment.
const END_LEN: u64 = 22;
const COMMENT_MAX: u64 = 65_535;
const CHUNK: usize = 64 * 1024;

fn u16_at(b: &[u8], i: usize) -> u64 {
    u16::from_le_bytes([b[i], b[i + 1]]) as u64
}

fn u32_at(b: &[u8], i: usize) -> u64 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap()) as u64
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().unwrap())
}

/// The file through a window that moves: local headers come one after
/// another, so most are read from the bytes the one before them brought in.
/// 70,000 entries are a thousand reads, not 70,000.
struct Window<'a> {
    file: &'a File,
    len: u64,
    buf: Vec<u8>,
    at: u64,
}

impl<'a> Window<'a> {
    fn new(file: &'a File, len: u64) -> Self {
        Window { file, len, buf: Vec::new(), at: 0 }
    }

    /// Up to `n` bytes at `off`: fewer only where the file ends.
    fn get(&mut self, off: u64, n: usize) -> io::Result<&[u8]> {
        let end = off.saturating_add(n as u64).min(self.len);
        if off >= end {
            return Ok(&[]);
        }
        let n = (end - off) as usize;
        if off < self.at || end > self.at + self.buf.len() as u64 {
            let want = n.max(CHUNK).min((self.len - off).min(usize::MAX as u64) as usize);
            self.buf.resize(want, 0);
            let mut filled = 0;
            while filled < want {
                match self.file.read_at(&mut self.buf[filled..], off + filled as u64) {
                    Ok(0) => break,
                    Ok(k) => filled += k,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
            }
            self.buf.truncate(filled);
            self.at = off;
        }
        let from = (off - self.at) as usize;
        Ok(&self.buf[from..(from + n).min(self.buf.len())])
    }

    fn starts(&mut self, off: u64, signature: &[u8]) -> io::Result<bool> {
        Ok(self.get(off, signature.len())? == signature)
    }

    /// Whether any byte of `from..to` is not zero.
    fn any_data(&mut self, mut from: u64, to: u64) -> io::Result<bool> {
        while from < to {
            let b = self.get(from, (to - from).min(CHUNK as u64) as usize)?;
            if b.is_empty() {
                break;
            }
            if b.iter().any(|&x| x != 0) {
                return Ok(true);
            }
            from += b.len() as u64;
        }
        Ok(false)
    }
}

/// What the end record says, the zip64 one where there is one.
struct End {
    /// Where the directory must have ended: at the end record, or at the
    /// zip64 end record before it.
    dir_end: u64,
    /// The first byte after the end record and its comment.
    after: u64,
    entries: u64,
    dir_size: u64,
    dir_offset: u64,
}

/// The last end record in the file. `exact`: only one that ends where the
/// file does. Otherwise zeros after it are padding, and anything else after
/// it is the caller's to report.
fn find_end(w: &mut Window<'_>, exact: bool) -> io::Result<Option<End>> {
    let len = w.len;
    // Past any zero padding. An end record's own last bytes are often zero,
    // so the search runs a record's length beyond the last byte that is not.
    let mut stop = len;
    while !exact && stop > 0 {
        let from = stop.saturating_sub(CHUNK as u64);
        let b = w.get(from, (stop - from) as usize)?;
        match b.iter().rposition(|&x| x != 0) {
            Some(i) => {
                stop = from + i as u64 + 1;
                break;
            }
            None => stop = from,
        }
    }
    let from = stop.saturating_sub(END_LEN + COMMENT_MAX);
    let to = (stop + END_LEN).min(len);
    let tail = w.get(from, (to - from) as usize)?.to_vec();
    for i in memchr::memmem::rfind_iter(&tail, END) {
        let Some(rec) = tail.get(i..i + END_LEN as usize) else { continue };
        let at = from + i as u64;
        let after = at + END_LEN + u16_at(rec, 20);
        if after > len || (exact && after != len) {
            continue;
        }
        let mut end = End {
            dir_end: at,
            after,
            entries: u16_at(rec, 10),
            dir_size: u32_at(rec, 12),
            dir_offset: u32_at(rec, 16),
        };
        if at >= 20 && w.starts(at - 20, LOCATOR64)? {
            zip64_end(w, at - 20, &mut end)?;
        }
        if end.dir_size <= end.dir_end {
            return Ok(Some(end));
        }
    }
    Ok(None)
}

/// The zip64 end record that ends where its locator starts. Found by its
/// own length and not by the offset the locator gives, which is as far off
/// as everything else in a zip with bytes in front of it.
fn zip64_end(w: &mut Window<'_>, locator: u64, end: &mut End) -> io::Result<()> {
    let from = locator.saturating_sub(CHUNK as u64);
    let before = w.get(from, (locator - from) as usize)?;
    for i in memchr::memmem::rfind_iter(before, END64) {
        let Some(rec) = before.get(i..i + 56) else { continue };
        if (i as u64 + 12).checked_add(u64_at(rec, 4)) == Some(before.len() as u64) {
            end.dir_end = from + i as u64;
            end.entries = u64_at(rec, 32);
            end.dir_size = u64_at(rec, 40);
            end.dir_offset = u64_at(rec, 48);
            break;
        }
    }
    Ok(())
}

/// One record of the central directory.
struct Record {
    /// The whole record's length, to the next one.
    len: u64,
    flags: u64,
    stored_size: u64,
    local_offset: u64,
}

fn record(w: &mut Window<'_>, at: u64) -> io::Result<Option<Record>> {
    let b = w.get(at, 46)?;
    if b.len() < 46 || &b[..4] != CENTRAL {
        return Ok(None);
    }
    let (name, extra, comment) = (u16_at(b, 28), u16_at(b, 30), u16_at(b, 32));
    let mut rec = Record {
        len: 46 + name + extra + comment,
        flags: u16_at(b, 8),
        stored_size: u32_at(b, 20),
        local_offset: u32_at(b, 42),
    };
    let size = u32_at(b, 24);
    const IN_ZIP64: u64 = 0xFFFF_FFFF;
    if rec.stored_size == IN_ZIP64 || size == IN_ZIP64 || rec.local_offset == IN_ZIP64 {
        // The zip64 extra field holds, in this order, only the values whose
        // 32-bit field is all ones.
        let mut x = w.get(at + 46 + name, extra as usize)?;
        while x.len() >= 4 {
            let (id, n) = (u16_at(x, 0), u16_at(x, 2) as usize);
            let Some(mut field) = x.get(4..4 + n) else { break };
            if id == 1 {
                let mut next = |is: bool, into: &mut u64| {
                    if is && field.len() >= 8 {
                        *into = u64_at(field, 0);
                        field = &field[8..];
                    }
                };
                next(size == IN_ZIP64, &mut 0);
                next(rec.stored_size == IN_ZIP64, &mut rec.stored_size);
                next(rec.local_offset == IN_ZIP64, &mut rec.local_offset);
                break;
            }
            x = &x[4 + n..];
        }
    }
    Ok(Some(rec))
}

/// Where the directory starts, and how far the zip's offsets are from the
/// file's: the length of whatever was put in front of it without adjusting
/// them (`cat stub a.zip`). The directory is looked for right before the end
/// record first, where every writer puts it, then at the offset the end
/// record gives. Either must hold a record whose entry is where it says.
fn locate(w: &mut Window<'_>, end: &End) -> io::Result<Option<(u64, u64)>> {
    if end.entries == 0 && end.dir_size == 0 {
        return Ok(Some((end.dir_end, 0)));
    }
    let adjacent = end.dir_end - end.dir_size;
    let candidates = [(adjacent, adjacent.checked_sub(end.dir_offset)), (end.dir_offset, Some(0))];
    for (start, base) in candidates {
        let (Some(base), Some(rec)) = (base, record(w, start)?) else { continue };
        let local = base.checked_add(rec.local_offset);
        if local.is_some_and(|at| w.starts(at, LOCAL).unwrap_or(false)) {
            return Ok(Some((start, base)));
        }
    }
    Ok(None)
}

/// Whether a file that does not start as a zip ends as one: a zip behind a
/// self-extractor or a script. Strict, as it is asked of every plain file:
/// an end record that ends exactly where the file does, leading to a
/// directory whose first entry is where it says.
pub fn ends_as_zip(file: &File, len: u64) -> bool {
    let mut w = Window::new(file, len);
    let found = find_end(&mut w, true).and_then(|end| match end {
        Some(end) => locate(&mut w, &end),
        None => Ok(None),
    });
    matches!(found, Ok(Some(_)))
}

/// What [`check`] found.
#[derive(Default)]
pub struct Checked {
    /// How many records the directory holds, whatever the end record counts.
    /// `None` when the directory could not be found.
    pub records: Option<u64>,
    /// Bytes that nothing the directory lists accounts for.
    pub errors: Vec<String>,
}

/// Measure the zip in `file` against its directory.
pub fn check(file: &File) -> Checked {
    let mut checked = Checked::default();
    if let Err(e) = measure(file, &mut checked) {
        checked.errors.push(format!("could not check the zip against its directory: {e}"));
    }
    checked
}

fn measure(file: &File, checked: &mut Checked) -> io::Result<()> {
    let len = file.metadata()?.len();
    let mut w = Window::new(file, len);
    let Some(end) = find_end(&mut w, false)? else {
        checked.errors.push(
            "the zip's end record is not within 64 KiB of its end: what follows it was not searched".into(),
        );
        return Ok(());
    };
    let Some((dir_start, base)) = locate(&mut w, &end)? else {
        checked.errors.push(
            "the zip's end record does not lead to its directory: the file could not be checked against it".into(),
        );
        return Ok(());
    };

    // Each entry's bytes: its local header, its data, and whether a data
    // descriptor follows.
    let mut entries: Vec<(u64, u64, bool)> = Vec::new();
    let mut records = 0u64;
    let mut at = dir_start;
    // A window of its own: the directory and the entries are read in step,
    // far apart, and one window would be refilled for every record.
    let mut dir = Window::new(file, len);
    while at < end.dir_end {
        let Some(rec) = record(&mut dir, at)? else { break };
        records += 1;
        at += rec.len;
        // An entry with no local header where the directory says is not
        // read either, and the reader says so itself.
        let Some(local) = base.checked_add(rec.local_offset) else { continue };
        let h = w.get(local, 30)?;
        if h.len() == 30 && &h[..4] == LOCAL {
            let data = local + 30 + u16_at(h, 26) + u16_at(h, 28);
            let described = (rec.flags | u16_at(h, 6)) & 8 != 0;
            entries.push((local, data.saturating_add(rec.stored_size), described));
        }
    }
    let dir_records_end = at;
    checked.records = Some(records);
    entries.sort_unstable();

    let mut gaps: Vec<(u64, u64)> = Vec::new();
    let first = entries.first().map_or(dir_start, |e| e.0);
    let mut covered = if first == 4 && w.starts(0, DESCRIPTOR)? { 4 } else { 0 };
    for (i, &(start, data_end, described)) in entries.iter().enumerate() {
        if start > covered {
            gaps.push((covered, start));
        }
        let next = entries.get(i + 1).map_or(dir_start, |e| e.0);
        let mut entry_end = data_end;
        if described && next > data_end {
            // CRC and two sizes, 4 or 8 bytes each, with or without a
            // signature: the one that reaches the next entry, or failing
            // that the shortest its first bytes allow.
            entry_end = match next - data_end {
                12 | 16 | 20 | 24 => next,
                _ if w.starts(data_end, DESCRIPTOR)? => next.min(data_end + 16),
                _ => next.min(data_end + 12),
            };
        }
        covered = covered.max(entry_end);
    }
    if dir_start > covered {
        gaps.push((covered, dir_start));
    }
    covered = covered.max(dir_records_end);
    if end.dir_end > covered {
        gaps.push((covered, end.dir_end));
    }
    if len > end.after {
        gaps.push((end.after, len));
    }

    let mut more = 0;
    for (from, to) in gaps {
        if !w.any_data(from, to)? {
            continue;
        }
        if checked.errors.len() >= 5 {
            more += 1;
            continue;
        }
        let n = to - from;
        let bytes = if n == 1 { "byte" } else { "bytes" };
        checked.errors.push(if from == 0 {
            format!(
                "{n} {bytes} before the zip's first entry were not searched \
                 (another zip, or a self-extractor, in front of this one?)"
            )
        } else if from == end.after {
            format!("{n} {bytes} after the zip's end record were not searched")
        } else {
            format!("{n} {bytes} at offset {from} are in no entry the zip's directory lists: not searched")
        });
    }
    if more > 0 {
        checked.errors.push(format!("and {more} more such places in the zip"));
    }
    Ok(())
}
