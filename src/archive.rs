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
/// basis for anything.)
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
    /// (`.Z`), so "read everything" is only as true as the file. Set by
    /// [`walk_archive`]; what to say about it is the caller's, since only
    /// the caller knows whether anything matched.
    pub unverified_end: bool,
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
        Opened::Stream(r, _) => Ok(r),
        Opened::Zip(..) => anyhow::bail!("a zip is not a stream; it is read through walk_archive"),
    }
}

/// An input, opened: a byte stream with its outer compression removed, or a
/// zip, which is read by seeking and so cannot be one.
enum Opened {
    /// The stream, and whether its format can detect its own truncation.
    Stream(Box<dyn Read>, bool),
    /// The file, rewound, and the guard that drops its pages when the walk is
    /// over.
    Zip(File, CacheGuard),
}

fn open_input(path: &Path, drop_cache: bool) -> anyhow::Result<Opened> {
    let f = File::open(path)?;
    let dup = f.try_clone()?;
    let rdr = io::BufReader::with_capacity(64 * 1024, f);
    let (head, body) = peek(rdr)?;

    let format = sniff(&head);
    let drop_cache = drop_cache && format != Format::Plain;
    crate::resource::prepare(&dup, drop_cache);
    let guard = CacheGuard { file: dup, drop_cache };

    let unverified_end = format == Format::CompressZ;
    let stream = match format {
        Format::Gzip => Guarded::boxed(flate2::read::MultiGzDecoder::new(body), guard),
        // `zstd`'s reader is streaming like flate2's, so the bounded-memory
        // guarantee and the cache guard carry over unchanged.
        Format::Zstd => Guarded::boxed(zstd_decoder(body)?, guard),
        // Multi-stream, like `MultiGzDecoder`: `cat a.xz b.xz` is a valid .xz
        // and `xz -d` decodes all of it.
        Format::Xz => Guarded::boxed(liblzma::read::XzDecoder::new_multi_decoder(body), guard),
        // Multi-stream too, and here it is not a corner case: pbzip2 and
        // lbzip2 write one stream per chunk, so a single-stream decoder would
        // stop after the first few hundred kilobytes and report the rest clean.
        Format::Bzip2 => Guarded::boxed(bzip2::read::MultiBzDecoder::new(body), guard),
        Format::Lz4 => Guarded::boxed(lz4_decoder(&head, body)?, guard),
        Format::CompressZ => Guarded::boxed(compress_z_decoder(body)?, guard),
        Format::Zip => {
            // Shares the open file description with the reader just dropped,
            // and so its offset: past the sniff, not at the start.
            drop(body);
            let mut file = guard.file.try_clone()?;
            io::Seek::rewind(&mut file)?;
            return Ok(Opened::Zip(file, guard));
        }
        Format::Tar | Format::Plain => Guarded::boxed(body, guard),
    };
    Ok(Opened::Stream(stream, unverified_end))
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
    F: FnMut(&str, &mut dyn Read) -> io::Result<()>,
{
    let mut out = match open_input(path, drop_cache) {
        Ok(Opened::Stream(r, unverified_end)) => Outcome {
            unverified_end,
            ..for_each_member(r, globs, f)
        },
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
        *e = format!("{}: {e}", path.display());
    }
    out
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
    F: FnMut(&str, &mut dyn Read) -> io::Result<()>,
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
                out.errors.push(format!("zip entry {index}: {e}"));
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
        match decode_member(&name, entry.reader(), &mut f) {
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
fn zstd_decoder<'a, R: Read>(r: R) -> io::Result<zstd::stream::read::Decoder<'a, io::BufReader<R>>> {
    let mut d = zstd::stream::read::Decoder::new(r)?;
    d.window_log_max(31)?;
    Ok(d)
}

/// An lz4 decoder positioned past any leading skippable frames, reading every
/// frame to the end of the input.
///
/// lz4_flex reads `lz4 -l` legacy frames by itself, but stops with an error
/// at a skippable frame. Only a leading one is stepped over here, which the
/// sniff has already measured; one between data frames is still an error, and
/// so still a 2 rather than a short read.
fn lz4_decoder<R: Read>(head: &[u8], mut r: R) -> io::Result<Lz4Frames<R>> {
    let skip = crate::sniff::skippable_prefix(head).unwrap_or(0) as u64;
    io::copy(&mut (&mut r).take(skip), &mut io::sink())?;
    Ok(Lz4Frames(lz4_flex::frame::FrameDecoder::new(io::BufReader::new(r))))
}

/// lz4_flex's decoder reports end-of-file at the end of every frame, so that
/// a caller can tell frames apart. `cat a.lz4 b.lz4` is one valid lz4 file,
/// and stopping at the first frame boundary silently drops the rest. A `0` is
/// passed on only when the input itself is exhausted; anything left means
/// another frame, which the next read starts.
struct Lz4Frames<R: Read>(lz4_flex::frame::FrameDecoder<io::BufReader<R>>);

impl<R: Read> Read for Lz4Frames<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        use io::BufRead;
        loop {
            let n = self.0.read(buf)?;
            if n > 0 || buf.is_empty() || self.0.get_mut().fill_buf()?.is_empty() {
                return Ok(n);
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
fn decode_member<F>(name: &str, member: &mut dyn Read, f: &mut F) -> io::Result<()>
where
    F: FnMut(&str, &mut dyn Read) -> io::Result<()>,
{
    let (inner_head, mut inner) = peek(member)?;
    match sniff(&inner_head) {
        Format::Gzip => f(name, &mut flate2::read::MultiGzDecoder::new(&mut inner)),
        Format::Zstd => f(name, &mut zstd_decoder(&mut inner)?),
        Format::Xz => f(name, &mut liblzma::read::XzDecoder::new_multi_decoder(&mut inner)),
        Format::Bzip2 => f(name, &mut bzip2::read::MultiBzDecoder::new(&mut inner)),
        Format::Lz4 => f(name, &mut lz4_decoder(&inner_head, &mut inner)?),
        // No unverified-end note here, unlike a whole `.Z` file: the archive
        // around the member records its length, and a cut archive is a 2.
        Format::CompressZ => f(name, &mut compress_z_decoder(&mut inner)?),
        _ => f(name, &mut inner),
    }
}

/// Walk every regular member, handing each to `f` as its own reader.
///
/// Never returns `Err`: a failure on one member is recorded in `Outcome.errors`
/// and iteration continues, because one bad archive must not abort the others.
pub fn for_each_member<R, F>(rdr: R, globs: Option<&crate::source::Globs>, mut f: F) -> Outcome
where
    R: Read,
    F: FnMut(&str, &mut dyn Read) -> io::Result<()>,
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
        match f("", &mut body) {
            Ok(()) => out.members_searched = 1,
            Err(e) => out.errors.push(format!("search failed: {e}")),
        }
        return out;
    }

    let mut ar = tar::Archive::new(body);
    let entries = match ar.entries() {
        Ok(e) => e,
        Err(e) => {
            out.errors.push(format!("not a readable tar: {e}"));
            return out;
        }
    };

    for entry in entries {
        let mut entry = match entry {
            Ok(e) => e,
            Err(e) => {
                // Truncated or corrupt: stop this archive, keep what we had.
                out.errors.push(format!("tar entry: {e}"));
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

        let res = decode_member(&name, &mut entry, &mut f);
        match res {
            Ok(()) => out.members_searched += 1,
            Err(e) => out.errors.push(format!("{name}: {e}")),
        }
    }

    out
}
