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
fn walk(root: &Path, errors: &mut Vec<String>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for res in ignore::WalkBuilder::new(root).standard_filters(false).build() {
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
