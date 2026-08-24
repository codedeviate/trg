//! Positional arguments become an ordered list of archives to search.
//! Order is preserved because these archives are days, so argument order is
//! chronological order.

use std::path::{Path, PathBuf};

pub fn resolve(paths: &[PathBuf]) -> (Vec<PathBuf>, Vec<String>) {
    let mut items = Vec::new();
    let mut errors = Vec::new();

    for p in paths {
        match std::fs::metadata(p) {
            Err(e) => errors.push(format!("{}: {e}", p.display())),
            Ok(md) if md.is_dir() => {
                let mut found = walk(p, &mut errors);
                found.sort();
                items.extend(found);
            }
            Ok(_) => items.push(p.clone()),
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
