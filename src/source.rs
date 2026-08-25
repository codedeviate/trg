//! Positional arguments become an ordered list of archives to search.
//! Order is preserved because these archives are days, so argument order is
//! chronological order.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

/// Identity used to de-duplicate resolved work items: on Unix, `(dev, ino)`,
/// which collapses both a symlink-and-its-target pair *and* hardlinks to the
/// same file down to a single search. `canonicalize()` would catch the
/// symlink case but not hardlinks -- and hardlinked rotated logs are a real
/// pattern this project has already been bitten by (GNU tar's inode-based
/// dedup of a hardlinked copy once produced a 696 KB archive that claimed to
/// hold 577 MB). Non-Unix targets have no `dev`/`ino`, so they fall back to a
/// canonicalized path -- macOS and Linux are the supported targets, so that
/// fallback only needs to keep the code compiling elsewhere, not be as
/// thorough.
#[cfg(unix)]
type Identity = (u64, u64);
#[cfg(not(unix))]
type Identity = PathBuf;

/// `None` means identity couldn't be determined (metadata read failed after
/// the fact, e.g. a TOCTOU removal). Callers must treat that as "assume
/// unique" -- better to search a file twice than to silently drop it.
#[cfg(unix)]
fn identity(p: &Path) -> Option<Identity> {
    std::fs::metadata(p).ok().map(|md| (md.dev(), md.ino()))
}

#[cfg(not(unix))]
fn identity(p: &Path) -> Option<Identity> {
    std::fs::canonicalize(p).ok()
}

/// Push `p` onto `items` unless something already pushed with the same file
/// identity got there first. Dedup is by first-occurrence in resolution
/// order, across the whole run -- not just within one directory walk -- so
/// `trg pat a.log a.log` and `trg pat dir/` (where `dir/` holds a file and a
/// symlink to it) both search the underlying file exactly once.
fn push_unique(items: &mut Vec<PathBuf>, seen: &mut HashSet<Identity>, p: PathBuf) {
    match identity(&p) {
        Some(id) if !seen.insert(id) => {} // already seen: drop this occurrence
        _ => items.push(p),
    }
}

pub fn resolve(paths: &[PathBuf]) -> (Vec<PathBuf>, Vec<String>) {
    let mut items = Vec::new();
    let mut errors = Vec::new();
    let mut seen = HashSet::new();

    for p in paths {
        match std::fs::metadata(p) {
            Err(e) => errors.push(format!("{}: {e}", p.display())),
            Ok(md) if md.is_dir() => {
                let mut found = walk(p, &mut errors);
                found.sort();
                for f in found {
                    push_unique(&mut items, &mut seen, f);
                }
            }
            Ok(_) => push_unique(&mut items, &mut seen, p.clone()),
        }
    }

    (items, errors)
}

/// `standard_filters(false)`: a log directory is not a source tree, so
/// `.gitignore`/hidden-file rules must not silently drop archives.
///
/// `follow_links(true)`: `/var/log` is full of symlinked and rotated-archive
/// symlinks, so `file_type()` must report the target's type rather than
/// `symlink`, or every symlinked log would be silently skipped by the
/// `is_file()` filter below. A broken symlink then surfaces as an `Err` from
/// the walk (the same as naming it directly would), which the `Err` arm
/// below records as an error rather than dropping it — a path's failure must
/// be reported the same way whether it was discovered by the walk or named
/// on the command line. `ignore` itself detects directory symlink cycles
/// (reported as an `Err`, not a hang) and a mutual file-symlink pair hits the
/// OS's own `ELOOP` first, also surfacing as an `Err` — see `tests/source.rs`
/// and the fix-round-1 section of the task report for how this was verified.
///
/// Following links means a symlink and its target (or two hardlinks) can
/// both be yielded here as distinct paths to the same file; `resolve`'s
/// `push_unique` is what collapses those back down to one work item.
fn walk(root: &Path, errors: &mut Vec<String>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for res in ignore::WalkBuilder::new(root)
        .standard_filters(false)
        .follow_links(true)
        .build()
    {
        match res {
            Ok(e) => {
                if e.file_type().map_or(false, |t| t.is_file()) {
                    out.push(e.into_path());
                }
            }
            Err(e) => errors.push(format!("{}: {e}", root.display())),
        }
    }
    out
}

pub fn build_globs(pats: &[String]) -> anyhow::Result<Option<globset::GlobSet>> {
    if pats.is_empty() {
        return Ok(None);
    }
    let mut b = globset::GlobSetBuilder::new();
    for p in pats {
        b.add(globset::Glob::new(p)?);
    }
    Ok(Some(b.build()?))
}
