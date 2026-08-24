//! Turns one input path into a sequence of independently-readable members.
//!
//! The load-bearing decision: every tar member is handed to the caller as its
//! own `Read`. That is what makes per-member line numbers, real member paths,
//! and per-member binary detection all fall out for free.

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

/// Open a path and strip one layer of compression if present.
pub fn open_decoded(path: &Path) -> anyhow::Result<Box<dyn Read + Send>> {
    let f = std::fs::File::open(path)?;
    let rdr = io::BufReader::with_capacity(64 * 1024, f);
    let (head, body) = peek(rdr)?;
    Ok(match sniff(&head) {
        Format::Gzip => Box::new(flate2::read::MultiGzDecoder::new(body)),
        Format::Tar | Format::Plain => Box::new(body),
    })
}

/// Like `open_decoded`, but lets the caller choose whole-buffer inflate.
///
/// `Strategy::Buffer` is only ever a fast path: a failure there (the ISIZE
/// hint lied and the retry loop in `inflate_buffered` still ran out of room)
/// falls back to `open_decoded`'s streaming path, which always works, and
/// never fails the search outright.
pub fn open_decoded_with(
    path: &Path,
    strategy: crate::inflate::Strategy,
) -> anyhow::Result<Box<dyn Read + Send>> {
    if let crate::inflate::Strategy::Buffer { capacity } = strategy {
        match crate::inflate::inflate_buffered(path, capacity) {
            Ok(bytes) => return Ok(Box::new(io::Cursor::new(bytes))),
            Err(e) => {
                // Never fail the search because the fast path did not fit:
                // fall back to streaming, which always works.
                eprintln!("trg: {}: buffered inflate fell back to streaming ({e})", path.display());
            }
        }
    }
    open_decoded(path)
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
