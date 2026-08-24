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
/// `errors` being non-empty is what marks the whole run partial (exit 2).
#[derive(Debug, Default)]
pub struct Outcome {
    pub members_seen: usize,
    pub members_searched: usize,
    pub errors: Vec<String>,
}

/// Drops an archive's page-cache footprint when the reader that owns it dies.
///
/// An RAII guard rather than an explicit call after the member walk, for two
/// reasons. It keeps `open_decoded`/`open_decoded_with` signatures unchanged,
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
pub fn open_decoded_dropping(
    path: &Path,
    strategy: crate::inflate::Strategy,
    drop_cache: bool,
) -> anyhow::Result<Box<dyn Read + Send>> {
    if let crate::inflate::Strategy::Buffer { capacity, ceiling } = strategy {
        match inflate_whole(path, capacity, ceiling, drop_cache) {
            Ok(bytes) => return Ok(Box::new(io::Cursor::new(bytes))),
            Err(e) => {
                // Never fail the search because the fast path did not fit:
                // fall back to streaming, which always works.
                eprintln!(
                    "trg: {}: buffered inflate fell back to streaming ({e})",
                    path.display()
                );
            }
        }
    }
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
        Format::Tar | Format::Plain => Guarded::boxed(body, guard),
    })
}

/// Whole-buffer inflate through a handle we control, so the compressed bytes
/// are read under the same cache policy as the streaming path. `fs::read`
/// inside `inflate::inflate_buffered` would open its own descriptor, which on
/// macOS could never carry `F_NOCACHE`.
///
/// The guard is dropped at the end of this function rather than travelling
/// with the returned `Cursor`: by then the file has been read whole and there
/// is nothing left to keep warm.
fn inflate_whole(
    path: &Path,
    capacity: usize,
    ceiling: usize,
    drop_cache: bool,
) -> io::Result<Vec<u8>> {
    let mut f = File::open(path)?;

    let mut raw = vec![0u8; SNIFF_LEN];
    let mut filled = 0;
    while filled < raw.len() {
        match f.read(&mut raw[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    raw.truncate(filled);

    let drop_cache = drop_cache && sniff(&raw) != Format::Plain;
    crate::resource::prepare(&f, drop_cache);
    let guard = CacheGuard { file: f.try_clone()?, drop_cache };

    f.read_to_end(&mut raw)?;
    let out = crate::inflate::inflate_bytes(&raw, path, capacity, ceiling);
    drop(guard);
    out
}

/// Like `open_decoded`, but lets the caller choose whole-buffer inflate.
///
/// `Strategy::Buffer` is only ever a fast path: a failure there — the ISIZE
/// hint lied and the retry loop in `inflate_buffered` hit the budget-derived
/// `ceiling` before it ran out of room, or the data was simply bad — falls
/// back to `open_decoded`'s streaming path, which always works, and never
/// fails the search outright. With a small `--inflate-budget` and a large
/// archive this fallback is the expected, common case, not an error: the
/// message `inflate_buffered` produces says so.
///
/// Signature frozen alongside `open_decoded`; see [`open_decoded_dropping`]
/// for the cache-dropping variant.
pub fn open_decoded_with(
    path: &Path,
    strategy: crate::inflate::Strategy,
) -> anyhow::Result<Box<dyn Read + Send>> {
    open_decoded_dropping(path, strategy, false)
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
fn peek<R: Read>(mut rdr: R) -> io::Result<(Vec<u8>, io::Chain<io::Cursor<Vec<u8>>, R>)> {
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
pub fn for_each_member<R, F>(rdr: R, globs: Option<&globset::GlobSet>, mut f: F) -> Outcome
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

        if let Some(set) = globs {
            if !set.is_match(&name) {
                continue;
            }
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

        let res = if sniff(&inner_head) == Format::Gzip {
            let mut dec = flate2::read::MultiGzDecoder::new(&mut inner);
            f(&name, &mut dec)
        } else {
            f(&name, &mut inner)
        };

        match res {
            Ok(()) => out.members_searched += 1,
            Err(e) => out.errors.push(format!("{name}: {e}")),
        }
    }

    out
}
