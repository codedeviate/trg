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

// The guard is only ever read at drop time; `File` is `Send`, so the whole
// wrapper is, which is what lets it box into `Box<dyn Read + Send>` for the
// worker threads of Task 10.
impl<R: Read + Send> Guarded<R> {
    fn boxed(inner: R, guard: CacheGuard) -> Box<dyn Read + Send>
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
pub fn open_decoded(path: &Path) -> anyhow::Result<Box<dyn Read + Send>> {
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
pub fn open_decoded_dropping(path: &Path, drop_cache: bool) -> anyhow::Result<Box<dyn Read + Send>> {
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
fn open_stream(path: &Path, drop_cache: bool) -> anyhow::Result<Box<dyn Read + Send>> {
    let f = File::open(path)?;
    let dup = f.try_clone()?;
    let rdr = io::BufReader::with_capacity(64 * 1024, f);
    let (head, body) = peek(rdr)?;

    let format = sniff(&head);
    let drop_cache = drop_cache && format != Format::Plain;
    crate::resource::prepare(&dup, drop_cache);
    let guard = CacheGuard { file: dup, drop_cache };

    Ok(match format {
        Format::Gzip => Guarded::boxed(flate2::read::MultiGzDecoder::new(body), guard),
        // `zstd`'s reader is streaming like flate2's, so the bounded-memory
        // guarantee and the cache guard carry over unchanged.
        Format::Zstd => Guarded::boxed(zstd_decoder(body)?, guard),
        // Multi-stream, like `MultiGzDecoder`: `cat a.xz b.xz` is a valid .xz
        // and `xz -d` decodes all of it.
        Format::Xz => Guarded::boxed(liblzma::read::XzDecoder::new_multi_decoder(body), guard),
        Format::Tar | Format::Plain => Guarded::boxed(body, guard),
    })
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

/// Read the sniff window, then hand back a reader with those bytes replayed in
/// front of the remainder.
///
/// The return type is concrete rather than `impl Read`, and carries no `Send`
/// bound: `Send`-ness is then inferred per call site. `open_decoded`'s
/// `BufReader<File>` chain is still `Send` (so it boxes into
/// `Box<dyn Read + Send>` for the worker threads of Task 10), while
/// `for_each_member` can also call this on a `&mut tar::Entry<'_, R>`, which is
/// not `Send`.
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

        // Exactly one level of nesting: logrotate puts access.log.1.gz inside
        // the tgz. Fixed at one level — that cap is the bomb guard.
        let (inner_head, mut inner) = match peek(&mut entry) {
            Ok(v) => v,
            Err(e) => {
                out.errors.push(format!("{name}: {e}"));
                continue;
            }
        };

        // Exactly one level, and every compression format — logrotate leaves
        // `access.log.1.gz` (or `.zst`, `.xz`) inside the archive, so a check that
        // named only gzip would silently hand a compressed member to the
        // searcher as binary.
        let res = match sniff(&inner_head) {
            Format::Gzip => {
                let mut dec = flate2::read::MultiGzDecoder::new(&mut inner);
                f(&name, &mut dec)
            }
            Format::Zstd => match zstd_decoder(&mut inner) {
                Ok(mut dec) => f(&name, &mut dec),
                Err(e) => Err(e),
            },
            Format::Xz => {
                let mut dec = liblzma::read::XzDecoder::new_multi_decoder(&mut inner);
                f(&name, &mut dec)
            }
            _ => f(&name, &mut inner),
        };

        match res {
            Ok(()) => out.members_searched += 1,
            Err(e) => out.errors.push(format!("{name}: {e}")),
        }
    }

    out
}
