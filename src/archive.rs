//! Turns one input path into a sequence of independently-readable members.
//!
//! The load-bearing decision: every tar member is handed to the caller as its
//! own `Read`. That is what makes per-member line numbers, real member paths,
//! and per-member binary detection all fall out for free.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use crate::sniff::{sniff, Format, SNIFF_LEN};

/// What happened while walking one archive.
///
/// `errors` being non-empty is what marks the whole run partial (exit 2), and
/// it is the **only** thing that does. In particular neither `members_seen !=
/// members_searched` nor `members_searched == 0` may be read as a partial run:
/// an archive whose members are all excluded by `-g` yields `seen = 2,
/// searched = 0, errors = []`, and it was read completely. (`members_searched`
/// also counts a glob-skipped plain file as searched, so it is not a sound
/// basis for anything.) That one archive is a clean 1; a whole run in which
/// `-g` kept nothing is a 2, decided in `main` from `kept`.
#[derive(Debug, Default)]
pub struct Outcome {
    pub members_seen: usize,
    pub members_searched: usize,
    pub errors: Vec<String>,
    /// Whether any member matched. Set by the caller — `for_each_member` has
    /// no opinion about what a match is — and it is the authoritative source
    /// for exit code 0, in place of "did this archive write any bytes".
    pub matched: bool,
    /// Advisory lines for stderr. Deliberately **not** `errors`: a note says
    /// the run read everything and printed less than it found, which is worth
    /// telling an operator and must not turn a 0 or a 1 into a 2.
    pub notes: Vec<String>,
    /// The archive's format cannot tell a complete file from a truncated one
    /// (`.Z`, or a stream ending in a legacy lz4 frame), so "read everything"
    /// is only as true as the file. The format's name, set by
    /// [`walk_archive`]; what to say about it is the caller's, since only
    /// the caller knows whether anything matched.
    pub unverified_end: Option<&'static str>,
    /// Whether any member got past `-g` (and, for a plain file, its path past
    /// `-g`). Set by the caller, which applies the plain-file half.
    pub kept: bool,
    /// With `--json`, the printer's statistics summed over the members, for
    /// the closing summary record.
    pub stats: Option<grep_printer::Stats>,
}

/// Drops an archive's page-cache footprint when the reader that owns it dies.
///
/// An RAII guard rather than an explicit call after the member walk, for two
/// reasons. It keeps the `open_decoded` signature unchanged,
/// so nothing that already builds on them has to move; and, more importantly,
/// it fires on the **error** paths too. A truncated archive — logrotate caught
/// mid-write, the case `tgz_truncated` exists for — bails out of
/// `for_each_member` early, and its pages are exactly the ones we still want
/// gone.
///
/// The handle here is a `dup` of the one the reader is using. Both share one
/// open file description, so `F_NOCACHE` set on either applies to both, and
/// `posix_fadvise` addresses the inode's pages regardless of which fd it is
/// called through.
struct CacheGuard {
    file: File,
    drop_cache: bool,
}

impl Drop for CacheGuard {
    fn drop(&mut self) {
        crate::resource::finish(&self.file, self.drop_cache);
    }
}

/// A reader that carries its `CacheGuard`. Field order is load-bearing: Rust
/// drops fields in declaration order, so `inner` releases the decompressor and
/// the underlying handle before `guard` issues `FADV_DONTNEED`.
struct Guarded<R> {
    inner: R,
    /// Never read — it exists only for its `Drop`. Hence the underscore.
    _guard: CacheGuard,
}

impl<R: Read> Read for Guarded<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

// Not `Send`: stuffr's decoders (`.Z`) are not, and nothing needs it, since
// each worker opens and reads its archive on its own thread.
impl<R: Read> Guarded<R> {
    fn boxed(inner: R, guard: CacheGuard) -> Box<dyn Read>
    where
        R: 'static,
    {
        Box::new(Guarded { inner, _guard: guard })
    }
}

/// Open a path and strip one layer of compression if present.
///
/// Signature frozen: Tasks 4 and 8 build on it. Cache-dropping behaviour is
/// reached through [`open_decoded_dropping`].
pub fn open_decoded(path: &Path) -> anyhow::Result<Box<dyn Read>> {
    open_stream(path, false)
}

/// `open_decoded`, plus the option to hand the archive's pages back to the
/// kernel once it has been read.
///
/// Cache-dropping is decided **here**, per input, and applies to archives
/// only: `Format::Plain` is left alone no matter what the operator asked for.
/// Apache holds its live logfiles open for append, and evicting those pages
/// would evict cache another process is actively using — precisely the harm
/// this feature exists to prevent.
pub fn open_decoded_dropping(path: &Path, drop_cache: bool) -> anyhow::Result<Box<dyn Read>> {
    open_stream(path, drop_cache)
}

/// The streaming open. `prepare` runs after the sniff rather than immediately
/// after `File::open`, because until the head has been read we do not know
/// whether this is an archive we are allowed to touch.
///
/// The cost of that ordering is paid on macOS, and it is larger than the
/// 512-byte sniff suggests: the file is wrapped in a 64 KiB `BufReader` before
/// `peek`, so the first read pulls **64 KiB — 16 pages — into the cache before
/// `F_NOCACHE` is set**. It is paid on *archives*, which are the only inputs
/// `F_NOCACHE` is ever applied to; plain files are excluded by design and stay
/// fully cached regardless. Sixteen pages per archive against the alternative
/// of setting `F_NOCACHE` on a live Apache log is the trade being made.
fn open_stream(path: &Path, drop_cache: bool) -> anyhow::Result<Box<dyn Read>> {
    match open_input(path, drop_cache)? {
        Opened::Stream(r, ..) => Ok(r),
        Opened::Zip(..) => anyhow::bail!("a zip is not a stream; it is read through walk_archive"),
    }
}

/// An input, opened: a byte stream with its outer compression removed, or a
/// zip, which is read by seeking and so cannot be one.
enum Opened {
    /// The stream, the format that cannot detect its own truncation if it
    /// is one (known for lz4 only once the stream has been read), whether it
    /// is compressed (and so has a checksum to read up to), and for a plain
    /// file, how to tell that it shrank while it was read.
    Stream(Box<dyn Read>, Unverified, bool, Option<Shrink>),
    /// The file, rewound, and the guard that drops its pages when the walk is
    /// over.
    Zip(File, CacheGuard),
}

/// The name of a format that cannot tell a cut-off file from a whole one,
/// shared with the reader that finds out which it is reading.
type Unverified = std::rc::Rc<std::cell::Cell<Option<&'static str>>>;

/// A plain file's size when it was opened, and a handle sharing its read
/// offset.
///
/// A live log can be truncated while trg reads it: logrotate's `copytruncate`
/// empties it in place. The read then meets the end of the file early and
/// looks like a whole file with no match, a 1 about lines never read. The
/// offset says how far the read got; short of the size at open, after a
/// search that read to the end, the file shrank under it.
struct Shrink {
    file: File,
    size: u64,
}

impl Shrink {
    fn check(&self) -> io::Result<()> {
        let at = io::Seek::stream_position(&mut &self.file)?;
        // And smaller now than then: a file whose stated size is not its
        // content, as sysfs's 4096 for a few bytes, ends early and has not
        // shrunk.
        if at < self.size && self.file.metadata()?.len() < self.size {
            return Err(io::Error::other(format!(
                "shrank while being read: {} bytes when opened, only {at} read \
                 (truncated in place, as by logrotate's copytruncate?)",
                self.size
            )));
        }
        Ok(())
    }
}

fn open_input(path: &Path, drop_cache: bool) -> anyhow::Result<Opened> {
    let f = if crate::source::is_stdin(path) { File::open("/dev/stdin")? } else { File::open(path)? };
    let md = f.metadata()?;
    let size = md.len();
    let dup = f.try_clone()?;
    let rdr = io::BufReader::with_capacity(64 * 1024, f);
    let (head, body) = peek(rdr)?;

    let format = sniff(&head);
    let asked = drop_cache;
    let drop_cache = drop_cache && format != Format::Plain;
    // Not for a zip: it is read by seeking, a header and an entry at a time,
    // and under macOS's F_NOCACHE every one of those small reads went to the
    // disk. 70,000 entries took 20 s instead of 0.3 s. Linux drops a zip's
    // pages after the walk, in `finish`, as for any archive.
    crate::resource::prepare(&dup, drop_cache && format != Format::Zip);
    let guard = CacheGuard { file: dup, drop_cache };

    let unverified_end = Unverified::default();
    if format == Format::CompressZ {
        unverified_end.set(Some(".Z"));
    }
    // Only a regular file has a size to fall short of. A pipe has none, and
    // asking it for its offset fails, which made `zcat x.gz | trg ERR -` with
    // no match a 2.
    let shrink = match format {
        Format::Plain if md.is_file() => Some(Shrink { file: guard.file.try_clone()?, size }),
        _ => None,
    };
    let stream = match format {
        Format::Gzip => Guarded::boxed(gzip_streams(body)?, guard),
        // `zstd`'s reader is streaming like flate2's, so the bounded-memory
        // guarantee and the cache guard carry over unchanged.
        Format::Zstd => Guarded::boxed(zstd_or_lz4(&head, body, Some(unverified_end.clone()))?, guard),
        // Multi-stream, like `MultiGzDecoder`: `cat a.xz b.xz` is a valid .xz
        // and `xz -d` decodes all of it.
        Format::Xz => Guarded::boxed(liblzma::read::XzDecoder::new_multi_decoder(body), guard),
        // Multi-stream too, and here it is not a corner case: pbzip2 and
        // lbzip2 write one stream per chunk, so a single-stream decoder would
        // stop after the first few hundred kilobytes and report the rest clean.
        Format::Bzip2 => Guarded::boxed(bzip2_streams(body)?, guard),
        Format::Lz4 => Guarded::boxed(lz4_decoder(&head, body, Some(unverified_end.clone()))?, guard),
        Format::CompressZ => Guarded::boxed(compress_z_decoder(body)?, guard),
        Format::Zip => {
            // Shares the open file description with the reader just dropped,
            // and so its offset: past the sniff, not at the start.
            drop(body);
            let mut file = guard.file.try_clone()?;
            if io::Seek::rewind(&mut file).is_err() {
                anyhow::bail!(
                    "a zip, which is read by seeking to its directory at the end, \
                     cannot be read from a pipe: save it to a file first"
                );
            }
            return Ok(Opened::Zip(file, guard));
        }
        Format::Tar => Guarded::boxed(body, guard),
        Format::Plain if asked => {
            let file = guard.file.try_clone()?;
            Guarded::boxed(BinaryCache { inner: body, file, binary: false }, guard)
        }
        Format::Plain => Guarded::boxed(body, guard),
    };
    Ok(Opened::Stream(stream, unverified_end, format != Format::Tar && format != Format::Plain, shrink))
}

/// A plain file's pages, given back once it turns out to be binary.
///
/// A plain file is kept in the cache: it may be a live log another process
/// is writing. But a search reads a binary file to its end, and a sweep of
/// /var/log would pull gigabytes of journals, wtmp and core dumps into the
/// cache, the harm the cache handling exists to prevent. A text log has no
/// NUL in it, so the first NUL settles it: from there macOS reads uncached,
/// and Linux drops the file's pages once it has been read. The one live log
/// with NULs, reopened at its old offset after copytruncate, is written by
/// appending, and loses no page its writer reads: `FADV_DONTNEED` drops
/// clean pages only.
struct BinaryCache<R> {
    inner: R,
    file: File,
    binary: bool,
}

impl<R: Read> Read for BinaryCache<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if !self.binary && memchr::memchr(0, &buf[..n]).is_some() {
            self.binary = true;
            crate::resource::prepare(&self.file, true);
        }
        Ok(n)
    }
}

impl<R> Drop for BinaryCache<R> {
    fn drop(&mut self) {
        if self.binary {
            crate::resource::finish(&self.file, true);
        }
    }
}

/// What follows a compressed stream that has ended: the end of the input
/// (`true`) or another stream (`false`), with any zero bytes before either
/// read through as block padding.
///
/// bsdtar, macOS's tar, writes `tar czf - dir > x.tgz` in 10 KiB blocks and
/// fills the last one with zeros. gzip, bzip2, zstd and lz4 decoders all read
/// those as the start of another stream and failed, a 2 for every archive
/// made that way on a Mac, where tar reads it whole. Only past a stream's
/// end: a stream's own trailer can end in zeros. No stream format here starts
/// with a zero byte, and whatever follows the zeros must still be one, so
/// `cat` of two padded files reads as both.
fn at_end<R: io::BufRead>(r: &mut R) -> io::Result<bool> {
    loop {
        let b = r.fill_buf()?;
        if b.is_empty() {
            return Ok(true);
        }
        let z = b.iter().take_while(|&&x| x == 0).count();
        if z < b.len() {
            r.consume(z);
            return Ok(false);
        }
        r.consume(z);
    }
}

/// One compressed stream after another, as `cat a.gz b.gz` is one valid .gz,
/// ending at the end of the input or at [`at_end`]'s padding.
struct Streams<R, D> {
    dec: Option<D>,
    rest: Option<R>,
    new: fn(R) -> io::Result<D>,
    end: fn(D) -> R,
}

impl<R: io::BufRead, D: Read> Streams<R, D> {
    fn new(r: R, new: fn(R) -> io::Result<D>, end: fn(D) -> R) -> io::Result<Self> {
        Ok(Streams { dec: Some(new(r)?), rest: None, new, end })
    }
}

impl<R: io::BufRead, D: Read> Read for Streams<R, D> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if let Some(d) = &mut self.dec {
                let n = d.read(buf)?;
                if n > 0 {
                    return Ok(n);
                }
                let d = self.dec.take().expect("checked above");
                self.rest = Some((self.end)(d));
            }
            let r = self.rest.as_mut().expect("a stream ended");
            if at_end(r)? {
                return Ok(0);
            }
            let r = self.rest.take().expect("checked above");
            self.dec = Some((self.new)(r)?);
        }
    }
}

type Buffered<R> = io::BufReader<R>;

fn gzip_streams<R: Read>(r: R) -> io::Result<impl Read> {
    Streams::new(
        Buffered::with_capacity(64 * 1024, r),
        |r| Ok(flate2::bufread::GzDecoder::new(r)),
        flate2::bufread::GzDecoder::into_inner,
    )
}

fn bzip2_streams<R: Read>(r: R) -> io::Result<impl Read> {
    Streams::new(
        Buffered::with_capacity(64 * 1024, r),
        |r| Ok(bzip2::bufread::BzDecoder::new(r)),
        bzip2::bufread::BzDecoder::into_inner,
    )
}

/// `.Z`, through stuffr's decoder.
///
/// The format has no length and no checksum, so a truncated `.Z` decodes
/// cleanly to a shorter file, wherever the cut falls: the one format here
/// where a 1 is only as good as the file it was read from. Hence
/// [`Outcome::unverified_end`], for a whole file and for a member alike.
fn compress_z_decoder<'a, R: Read + 'a>(r: R) -> io::Result<Box<dyn stuffr_core::Source + 'a>> {
    stuffr_core::decode_reader(&stuffr_formats::legacy::compress_z::CompressZ, r, &Default::default())
        .map_err(io::Error::other)
}

/// What to do with the rest of a member once the callback is done with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rest {
    /// Leave it: the callback has what it came for, a match or a listing.
    Leave,
    /// Read it to the end. A search that found nothing says this, because its
    /// 1 claims everything was read: a member that looked binary was left at
    /// its first NUL, short of the checksum at its end. Only data with a
    /// checksum is read on (a decoder, a zip entry), so an endless plain
    /// stream such as /dev/zero still stops at its first NUL.
    Verify,
}

/// Read the rest of `r` when `rest` asks for it.
fn finish(rest: Rest, r: &mut dyn Read) -> io::Result<()> {
    if rest == Rest::Verify {
        io::copy(r, &mut io::sink())?;
    }
    Ok(())
}

/// Open `path` and hand each member to `f`, whatever kind of archive it is:
/// a tar under any one compression, a zip, or a single compressed or plain
/// file as one anonymous member. Never returns `Err`, for the reason
/// [`for_each_member`] gives; a file that cannot be opened is an error on its
/// [`Outcome`].
pub fn walk_archive<F>(
    path: &Path,
    drop_cache: bool,
    globs: Option<&crate::source::Globs>,
    f: F,
) -> Outcome
where
    F: FnMut(&str, &mut dyn Read) -> io::Result<Rest>,
{
    let mut out = match open_input(path, drop_cache) {
        Ok(Opened::Stream(r, unverified_end, compressed, shrink)) => {
            // Read after the walk: only the end of an lz4 stream says
            // whether it ended in a legacy frame.
            let out = walk_stream(r, globs, compressed, shrink, f);
            Outcome { unverified_end: unverified_end.get(), ..out }
        }
        // The guard outlives the walk, so the pages go once it is over.
        Ok(Opened::Zip(file, _guard)) => for_each_zip_member(file, globs, f),
        Err(e) => {
            let mut o = Outcome::default();
            o.errors.push(e.to_string());
            o
        }
    };
    // Named here, once, for every way a walk can fail. A sweep of a month of
    // archives that says only "unexpected end of file" has not said which
    // one to look at.
    for e in &mut out.errors {
        *e = format!("{}: {e}", crate::source::display(path));
    }
    out
}

/// stuffr's reason a zip entry could not be read, said the way trg can act
/// on it. An encrypted entry's text suggests building stuffr with zip's
/// `aes-crypto` feature, advice for stuffr's own command line: trg takes no
/// password, so no build of it would read the entry, and ZipCrypto entries
/// are not AES at all.
///
/// Matched on zip's own text, `ZipError::PASSWORD_REQUIRED`; a test holds
/// the two together, as zip is not a dependency of trg itself.
const PASSWORD_REQUIRED: &str = "Password required to decrypt file";

fn zip_entry_error(e: &stuffr_core::Error) -> String {
    let text = e.to_string();
    if text.contains(PASSWORD_REQUIRED) {
        return "encrypted, and trg takes no password: not searched".into();
    }
    text
}

/// Walk a zip's members through stuffr's zip container.
///
/// Read from the central directory, by seeking, which is why a zip goes
/// through here and not through the streaming path: the local headers alone
/// can leave sizes unknown, and the directory at the end is the archive's own
/// account of what it holds. Only a whole file can be read this way, so a zip
/// inside another archive is searched as bytes, like any member that is not
/// compressed text.
///
/// Anything stuffr reports about the archive itself — records it could not
/// account for, such as two sharing one name — lands in `errors`, not
/// `notes`: those are members that were not searched.
fn for_each_zip_member<F>(file: File, globs: Option<&crate::source::Globs>, mut f: F) -> Outcome
where
    F: FnMut(&str, &mut dyn Read) -> io::Result<Rest>,
{
    use stuffr_core::{Container, EntryKind};
    use stuffr_formats::zip::{Zip, ZIP};

    let mut out = Outcome::default();
    let opened = stuffr_core::FileSource::from_file(file).and_then(|src| {
        let resolved =
            stuffr_core::resolve(Box::new(src), ZIP, Zip.caps(), &stuffr_core::StreamPolicy::Exact)?;
        Zip.open(resolved, &Default::default())
    });
    let mut archive = match opened {
        Ok(a) => a,
        Err(e) => {
            out.errors.push(format!("not a readable zip: {e}"));
            return out;
        }
    };

    // Records stuffr failed to hand back in a row. Read by index from the
    // central directory, each call moves on to the next record whether or not
    // this one could be read — an encrypted member is one bad record, not the
    // end of the archive — but that is stuffr's behaviour and not its
    // contract, so a run of failures this long ends the walk rather than
    // trusting it to.
    const GIVE_UP_AFTER: usize = 1000;
    let mut failed_in_a_row = 0;
    let mut index = 0usize;
    loop {
        index += 1;
        let mut entry = match archive.next_entry() {
            Ok(Some(e)) => e,
            Ok(None) => break,
            Err(e) => {
                out.errors.push(format!("zip entry {index}: {}", zip_entry_error(&e)));
                failed_in_a_row += 1;
                if failed_in_a_row >= GIVE_UP_AFTER {
                    out.errors.push(format!("gave up after {GIVE_UP_AFTER} unreadable zip entries in a row"));
                    break;
                }
                continue;
            }
        };
        failed_in_a_row = 0;
        if !matches!(entry.meta().kind, EntryKind::File) {
            continue;
        }
        let name = entry.meta().name.clone();
        out.members_seen += 1;
        if globs.is_some_and(|set| !set.is_match(&name)) {
            continue;
        }
        // A zip entry carries its own CRC-32, stored or not, checked at its end.
        match decode_member(&name, entry.reader(), &mut f).and_then(|rest| finish(rest, entry.reader())) {
            Ok(()) => out.members_searched += 1,
            Err(e) => out.errors.push(format!("{name}: {e}")),
        }
    }

    // Among them a cloaked zip (GHSA-m665-v7pm-7grj), whose local header
    // names one file and its central directory another. trg searched what the
    // central directory names, as unzip would, but which was meant is not
    // knowable.
    for w in &archive.fidelity().warnings {
        out.errors.push(w.to_string());
    }
    out
}

/// A zstd decoder that accepts any window the format allows, up to 2^31.
///
/// The decoder's default ceiling is 2^27 (128 MB), the same as the `zstd`
/// binary's, and archives made with `zstd --long=28` or more were refused:
/// "Frame requires too much memory for decoding", exit 2. The window is chosen
/// by whoever made the archive, so refusing it leaves those archives
/// unsearchable; trg has no flag to raise it because nobody can know they need
/// one until an incident. The memory is only ever as large as the archive's
/// own frame header declares.
///
/// One frame per decoder, in [`Streams`], so the padding after the last one
/// is read as padding.
fn zstd_decoder<R: Read>(r: R) -> io::Result<impl Read> {
    fn frame<'a, B: io::BufRead>(r: B) -> io::Result<zstd::stream::read::Decoder<'a, B>> {
        let mut d = zstd::stream::read::Decoder::with_buffer(r)?.single_frame();
        d.window_log_max(31)?;
        Ok(d)
    }
    Streams::new(Buffered::with_capacity(64 * 1024, r), frame, zstd::stream::read::Decoder::finish)
}

/// What follows a skippable frame too long for the sniff window to see past:
/// zstd, or lz4, which shares the skippable magics.
///
/// The sniff calls such a head zstd, the format that writes them in practice,
/// and an lz4 frame behind one was "Unknown frame descriptor", a 2 for a
/// file `lz4 -d` reads. So the leading skippable frames are read through
/// here, on the stream, and the magic after them decides. A head the sniff
/// saw past goes to zstd untouched, and what is not lz4 goes to it behind an
/// empty skippable frame in place of those read: the zstd decoder then sees
/// what it always did, a skippable frame and whatever follows, even nothing.
fn zstd_or_lz4<R: Read>(head: &[u8], mut r: R, legacy_end: Option<Unverified>) -> io::Result<impl Read + use<R>> {
    let mut magic = Vec::new();
    let mut stepped = false;
    if crate::sniff::skippable_prefix(head).is_none() {
        stepped = true;
        loop {
            magic.clear();
            (&mut r).take(4).read_to_end(&mut magic)?;
            // Of four bytes, only a skippable magic has a frame running on.
            if magic.len() < 4 || crate::sniff::skippable_prefix(&magic).is_some() {
                break;
            }
            let mut len = [0u8; 4];
            r.read_exact(&mut len)?;
            let len = u32::from_le_bytes(len) as u64;
            if io::copy(&mut (&mut r).take(len), &mut io::sink())? < len {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "cut off inside a skippable frame"));
            }
        }
    }
    let lz4 = sniff(&magic) == Format::Lz4;
    if stepped && !lz4 {
        magic.splice(..0, [0x50, 0x2A, 0x4D, 0x18, 0, 0, 0, 0]);
    }
    let r = io::Cursor::new(magic).chain(r);
    Ok(if lz4 {
        ZstdOrLz4::Lz4(lz4_decoder(&[], r, legacy_end)?)
    } else {
        ZstdOrLz4::Zstd(zstd_decoder(r)?)
    })
}

enum ZstdOrLz4<Z, L> {
    Zstd(Z),
    Lz4(L),
}

impl<Z: Read, L: Read> Read for ZstdOrLz4<Z, L> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            ZstdOrLz4::Zstd(z) => z.read(buf),
            ZstdOrLz4::Lz4(l) => l.read(buf),
        }
    }
}

/// An lz4 decoder positioned past any leading skippable frames, reading every
/// frame to the end of the input.
///
/// lz4_flex reads `lz4 -l` legacy frames by itself, but stops with an error
/// at a skippable frame. Only a leading one is stepped over here, which the
/// sniff has already measured; one between data frames is still an error, and
/// so still a 2 rather than a short read.
///
/// `legacy_end` is told when the stream turns out to end in a legacy frame.
fn lz4_decoder<R: Read>(head: &[u8], mut r: R, legacy_end: Option<Unverified>) -> io::Result<Lz4Frames<R>> {
    let skip = crate::sniff::skippable_prefix(head).unwrap_or(0) as u64;
    io::copy(&mut (&mut r).take(skip), &mut io::sink())?;
    let input = io::BufReader::new(crate::lz4_track::Lz4Track::new(r));
    Ok(Lz4Frames { dec: Some(lz4_flex::frame::FrameDecoder::new(input)), legacy_end })
}

/// lz4_flex's decoder reports end-of-file at the end of every frame, so that
/// a caller can tell frames apart. `cat a.lz4 b.lz4` is one valid lz4 file,
/// and stopping at the first frame boundary silently drops the rest. A `0` is
/// passed on only when the input itself is exhausted; anything left means
/// another frame, which the next read starts.
///
/// lz4_flex also reports end-of-file when the input ends between two blocks
/// of an unfinished frame, so at the end of the input the framing is checked
/// too: a stream cut off mid-frame is an error, not a shorter file
/// (<https://github.com/PSeitz/lz4_flex/issues/246>).
///
/// A legacy frame ends only where its input does, and the decoder would read
/// the magic of a frame after it as a block size. [`Lz4Track`] ends the input
/// before that magic instead, and a new decoder starts on it: the old one
/// still thinks itself inside the legacy frame.
///
/// [`Lz4Track`]: crate::lz4_track::Lz4Track
struct Lz4Frames<R: Read> {
    /// `None` only while one decoder is exchanged for the next.
    dec: Option<lz4_flex::frame::FrameDecoder<io::BufReader<crate::lz4_track::Lz4Track<R>>>>,
    legacy_end: Option<Unverified>,
}

impl<R: Read> Read for Lz4Frames<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        use io::BufRead;
        loop {
            let dec = self.dec.as_mut().expect("a decoder between reads");
            let n = dec.read(buf)?;
            if n > 0 || buf.is_empty() {
                return Ok(n);
            }
            let input = dec.get_mut();
            if input.fill_buf()?.is_empty() && input.get_ref().at_frame_start() {
                let mut input = self.dec.take().expect("checked above").into_inner();
                input.get_mut().resume();
                self.dec = Some(lz4_flex::frame::FrameDecoder::new(input));
                continue;
            }
            if input.fill_buf()?.is_empty() || at_end(input)? {
                let t = input.get_ref();
                if t.lost() {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "data after the lz4 stream that is not lz4"));
                }
                if t.mid_frame() {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "lz4 stream cut off mid-frame"));
                }
                if let (true, Some(end)) = (t.ends_legacy(), &self.legacy_end) {
                    end.set(Some("the legacy lz4 format (lz4 -l)"));
                }
                return Ok(0);
            }
        }
    }
}

/// Read the sniff window, then hand back a reader with those bytes replayed in
/// front of the remainder.
///
/// The return type is concrete rather than `impl Read`, so `for_each_member`
/// can also call this on a `&mut tar::Entry<'_, R>`.
/// The sniffed head, and a reader that replays it before the rest.
type Peeked<R> = (Vec<u8>, io::Chain<io::Cursor<Vec<u8>>, R>);

fn peek<R: Read>(mut rdr: R) -> io::Result<Peeked<R>> {
    let mut head = vec![0u8; SNIFF_LEN];
    let mut filled = 0;
    while filled < head.len() {
        match rdr.read(&mut head[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    head.truncate(filled);
    let replay = io::Cursor::new(head.clone());
    Ok((head, replay.chain(rdr)))
}

/// Hand one member to `f`, with one level of compression removed.
///
/// Exactly one level of nesting: logrotate puts `access.log.1.gz` inside the
/// archive. Fixed at one level — that cap is the bomb guard. Every format is
/// checked, because a check that named only gzip would silently hand a
/// compressed member to the searcher as binary.
///
/// Returns what `f` said about the rest of the member, having already read a
/// compressed member's rest when `f` asked for it.
fn decode_member<F>(name: &str, member: &mut dyn Read, f: &mut F) -> io::Result<Rest>
where
    F: FnMut(&str, &mut dyn Read) -> io::Result<Rest>,
{
    let (inner_head, mut inner) = peek(member)?;
    let mut decoded = |d: &mut dyn Read| -> io::Result<Rest> {
        let rest = f(name, d)?;
        finish(rest, d)?;
        Ok(rest)
    };
    match sniff(&inner_head) {
        Format::Gzip => decoded(&mut gzip_streams(&mut inner)?),
        Format::Zstd => decoded(&mut zstd_or_lz4(&inner_head, &mut inner, None)?),
        Format::Xz => decoded(&mut liblzma::read::XzDecoder::new_multi_decoder(&mut inner)),
        Format::Bzip2 => decoded(&mut bzip2_streams(&mut inner)?),
        Format::Lz4 => decoded(&mut lz4_decoder(&inner_head, &mut inner, None)?),
        // No unverified-end note here, unlike a whole `.Z` file: the archive
        // around the member records its length, and a cut archive is a 2.
        Format::CompressZ => decoded(&mut compress_z_decoder(&mut inner)?),
        _ => f(name, &mut inner),
    }
}

/// Walk every regular member, handing each to `f` as its own reader.
///
/// Never returns `Err`: a failure on one member is recorded in `Outcome.errors`
/// and iteration continues, because one bad archive must not abort the others.
pub fn for_each_member<R, F>(rdr: R, globs: Option<&crate::source::Globs>, f: F) -> Outcome
where
    R: Read,
    F: FnMut(&str, &mut dyn Read) -> io::Result<Rest>,
{
    walk_stream(rdr, globs, false, None, f)
}

/// [`for_each_member`], told whether `rdr` is a decoder, whose rest is read
/// when a whole file that is not a tar asks for [`Rest::Verify`].
fn walk_stream<R, F>(
    rdr: R,
    globs: Option<&crate::source::Globs>,
    compressed: bool,
    shrink: Option<Shrink>,
    mut f: F,
) -> Outcome
where
    R: Read,
    F: FnMut(&str, &mut dyn Read) -> io::Result<Rest>,
{
    let mut out = Outcome::default();

    let (head, mut body) = match peek(rdr) {
        Ok(v) => v,
        Err(e) => {
            out.errors.push(format!("read failed: {e}"));
            return out;
        }
    };

    // Not a tar at all: the whole input is a single anonymous member.
    if sniff(&head) != Format::Tar {
        out.members_seen = 1;
        let mut seen = SawEnd { inner: &mut body, ended: false };
        let searched = f("", &mut seen).and_then(|rest| {
            let ended = seen.ended;
            match (compressed, &shrink) {
                (true, _) => finish(rest, &mut body),
                // Read to its end with no match: all of it, unless it
                // shrank. Only once the read met the end: a search that
                // stopped at binary data left the rest unread on purpose,
                // and every binary file over 64 KiB, /var/log/wtmp among
                // them, was a 2 that said it had shrunk.
                (false, Some(s)) if rest == Rest::Verify && ended => s.check(),
                (false, _) => Ok(()),
            }
        });
        match searched {
            Ok(()) => out.members_searched = 1,
            Err(e) => out.errors.push(format!("search failed: {e}")),
        }
        return out;
    }

    let mut ar = tar::Archive::new(EndMark { inner: body, zeros: 0 });
    // Read on past the end-of-archive blocks, as `tar -i` does. `cat
    // day1.tgz day2.tgz > week.tgz` is one valid gzip holding two tars, and
    // stopping at the first one's end left every member of the second
    // unsearched: a 1 about a day never read. Anything after the end that is
    // not zeros or another tar is now an error, where it was read and thrown
    // away.
    ar.set_ignore_zeros(true);
    let entries = match ar.entries() {
        Ok(e) => e,
        Err(e) => {
            out.errors.push(format!("not a readable tar: {e}"));
            return out;
        }
    };

    let mut ended = true;
    for entry in entries {
        let mut entry = match entry {
            Ok(e) => e,
            Err(e) => {
                // Truncated or corrupt: stop this archive, keep what we had.
                out.errors.push(format!("tar entry: {e}"));
                ended = false;
                break;
            }
        };

        // `is_file()` is exactly `== Regular`, which excludes hardlinks,
        // symlinks, dirs, devices, FIFOs, GNU sparse, @LongLink and pax
        // headers in one check. Hardlinks in particular carry no content.
        if !entry.header().entry_type().is_file() {
            continue;
        }

        let name = match entry.path() {
            Ok(p) => p.to_string_lossy().into_owned(),
            Err(e) => {
                out.errors.push(format!("bad member path: {e}"));
                continue;
            }
        };

        out.members_seen += 1;

        if globs.is_some_and(|set| !set.is_match(&name)) {
            continue;
        }

        // A plain member's rest needs no reading here: the tar reads past it
        // to the next header anyway, through the decoder and its checksum.
        let res = decode_member(&name, &mut entry, &mut f);
        match res {
            Ok(_) => out.members_searched += 1,
            Err(e) => out.errors.push(format!("{name}: {e}")),
        }
    }

    // The tar ends at its end-of-archive blocks, but the compressed stream
    // around it goes on to its trailer: gzip's CRC-32 and length, bzip2's
    // stream CRC, lz4's content checksum. Stopping at the tar's end never
    // read them, and a corrupt archive could come back a 0 with shifted line
    // numbers, or a 1. What is left is padding and the trailer.
    if ended {
        let mut rest = ar.into_inner();
        if let Err(e) = io::copy(&mut rest, &mut io::sink()) {
            out.errors.push(format!("after the tar's end: {e}"));
        } else if rest.zeros < 512 {
            out.errors.push("the tar ends without its end-of-archive blocks: cut short".into());
        }
    }

    out
}

/// Whether a read has returned the end of the input.
struct SawEnd<'a> {
    inner: &'a mut dyn Read,
    ended: bool,
}

impl Read for SawEnd<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n == 0 && !buf.is_empty() {
            self.ended = true;
        }
        Ok(n)
    }
}

/// Counts the zero bytes the stream ends in. The tar reader takes the end of
/// its input at a header boundary for the end of the archive, so a plain tar
/// cut between two members read as a whole, shorter one, and a search of it
/// was a 1 about members never read. Every tar writer ends the archive with
/// zero blocks; a stream that does not end in at least one was cut.
struct EndMark<R> {
    inner: R,
    zeros: u64,
}

impl<R: Read> Read for EndMark<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        let b = &buf[..n];
        self.zeros = match b.iter().rposition(|&x| x != 0) {
            Some(i) => (n - 1 - i) as u64,
            None => self.zeros + n as u64,
        };
        Ok(n)
    }
}

#[cfg(test)]
mod zip_error_tests {
    #[test]
    fn the_password_text_is_zips() {
        assert_eq!(super::PASSWORD_REQUIRED, zip::result::ZipError::PASSWORD_REQUIRED);
    }
}

#[cfg(test)]
mod padding_tests {
    use super::*;

    fn gz(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        io::Write::write_all(&mut e, data).unwrap();
        e.finish().unwrap()
    }

    fn read_all(r: io::Result<impl Read>) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        r?.read_to_end(&mut out)?;
        Ok(out)
    }

    /// 10240 bytes in, so the gzip trailer's length ends in two zeros: they
    /// are the trailer's, not padding.
    #[test]
    fn zeros_after_the_last_stream_are_padding_and_a_trailers_own_are_not() {
        let data = vec![b'x'; 10240];
        let mut two = gz(&data);
        two.extend(gz(b"tail"));
        let mut want = data.clone();
        want.extend_from_slice(b"tail");
        assert_eq!(read_all(gzip_streams(&two[..])).unwrap(), want);
        let mut padded = two.clone();
        padded.resize(two.len() + 9000, 0);
        assert_eq!(read_all(gzip_streams(&padded[..])).unwrap(), want);
        padded.push(1);
        assert!(read_all(gzip_streams(&padded[..])).is_err(), "data after the padding");
        padded.pop();
        padded.extend(gz(b"!"));
        want.push(b'!');
        assert_eq!(read_all(gzip_streams(&padded[..])).unwrap(), want, "a stream after the padding");
        let cut = &two[..two.len() - 1];
        assert!(read_all(gzip_streams(cut)).is_err(), "a cut trailer");
    }
}

#[cfg(test)]
mod shrink_tests {
    use super::*;
    use std::io::Write;

    /// A file truncated in place after it was opened, read to its new end:
    /// short of the size at open, so not all of it was read.
    #[test]
    fn a_file_that_shrank_while_being_read_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("live.log");
        std::fs::write(&p, vec![b'x'; 10_000]).unwrap();
        let mut f = File::open(&p).unwrap();
        let s = Shrink { file: f.try_clone().unwrap(), size: f.metadata().unwrap().len() };
        let mut head = [0u8; 4096];
        f.read_exact(&mut head).unwrap();
        std::fs::OpenOptions::new().write(true).open(&p).unwrap().set_len(0).unwrap();
        io::copy(&mut f, &mut io::sink()).unwrap();
        let e = s.check().unwrap_err().to_string();
        assert!(e.contains("10000 bytes when opened, only 4096 read"), "{e}");

        // Read whole, or grown since: fine.
        std::fs::write(&p, vec![b'x'; 100]).unwrap();
        let mut f = File::open(&p).unwrap();
        let s = Shrink { file: f.try_clone().unwrap(), size: 100 };
        std::fs::OpenOptions::new().append(true).open(&p).unwrap().write_all(b"more").unwrap();
        io::copy(&mut f, &mut io::sink()).unwrap();
        s.check().unwrap();
    }

    /// Through the walk: a search that reads to the (new) end of a file cut
    /// under it is an error, and one that stops early on purpose is not.
    #[test]
    fn the_walk_checks_only_a_read_that_met_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("live.log");
        let walk = |stop_early: bool| {
            std::fs::write(&p, vec![b'x'; 200_000]).unwrap();
            let mut cut = false;
            walk_archive(&p, false, None, |_, r| {
                let mut buf = [0u8; 4096];
                r.read_exact(&mut buf)?;
                if !cut {
                    std::fs::OpenOptions::new().write(true).open(&p)?.set_len(0)?;
                    cut = true;
                }
                if !stop_early {
                    io::copy(r, &mut io::sink())?;
                }
                Ok(Rest::Verify)
            })
        };
        let read_on = walk(false);
        assert!(read_on.errors.iter().any(|e| e.contains("shrank")), "{:?}", read_on.errors);
        let stopped = walk(true);
        assert!(stopped.errors.is_empty(), "{:?}", stopped.errors);
    }
}
