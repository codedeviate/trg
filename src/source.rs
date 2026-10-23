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
    resolve_with(paths, None)
}

/// `resolve`, keeping only the walked files that `archive_globs` selects.
///
/// Files named on the command line are never filtered, as with `rg -g`: naming
/// a file is a stronger statement than any pattern.
///
/// A glob that selects nothing anywhere in the walk is an error, not an empty
/// search: the run would otherwise exit 1, the claim that everything asked for
/// was read and nothing matched, and a typo like `2026-8-*` would read as
/// clean logs. It is judged across the whole run, so one directory without a
/// match is normal when another has them.
pub fn resolve_with(
    paths: &[PathBuf],
    archive_globs: Option<&ArchiveGlobs>,
) -> (Vec<PathBuf>, Vec<String>) {
    let mut items = Vec::new();
    let mut errors = Vec::new();
    let mut seen = HashSet::new();
    let mut walked_any = false;
    let mut kept_any = false;

    for p in paths {
        match std::fs::metadata(p) {
            Err(e) => errors.push(format!("{}: {e}", p.display())),
            Ok(md) if md.is_dir() => {
                walked_any = true;
                let mut found = walk(p, &mut errors);
                if let Some(g) = archive_globs {
                    found.retain(|f| g.is_match(p, f));
                }
                kept_any |= !found.is_empty();
                found.sort();
                for f in found {
                    push_unique(&mut items, &mut seen, f);
                }
            }
            Ok(_) => push_unique(&mut items, &mut seen, p.clone()),
        }
    }

    if archive_globs.is_some() && walked_any && !kept_any {
        errors.push("--archive-glob matched no files in the directories searched".to_string());
    }

    (items, errors)
}

/// How an ordered list of globs decides, the way ripgrep and `.gitignore`
/// read one: a glob starting with `!` excludes, and when several match, the
/// last one given wins. If any glob is not an exclusion, a candidate matching
/// none of them is left out — the list is a whitelist with exceptions.
/// Otherwise it is a blacklist, and a candidate matching none is kept.
struct Rules {
    negated: Vec<bool>,
    any_positive: bool,
}

impl Rules {
    /// `last` is the highest index, in command-line order, of the globs that
    /// matched.
    fn keep(&self, last: Option<usize>) -> bool {
        match last {
            Some(i) => !self.negated[i],
            None => !self.any_positive,
        }
    }
}

/// Split a leading `!` off. `\!` still means a literal `!`: globset reads the
/// backslash as an escape.
fn parse_rule(p: &str) -> (bool, &str) {
    match p.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, p),
    }
}

/// `-g`: which members are searched.
pub struct Globs {
    set: globset::GlobSet,
    rules: Rules,
}

impl Globs {
    pub fn is_match(&self, candidate: impl AsRef<Path>) -> bool {
        self.rules.keep(self.set.matches(candidate.as_ref()).into_iter().max())
    }
}

pub fn build_globs(pats: &[String]) -> anyhow::Result<Option<Globs>> {
    if pats.is_empty() {
        return Ok(None);
    }
    let mut b = globset::GlobSetBuilder::new();
    let mut negated = Vec::new();
    for p in pats {
        let (neg, glob) = parse_rule(p);
        b.add(globset::Glob::new(glob)?);
        negated.push(neg);
    }
    let any_positive = negated.contains(&false);
    Ok(Some(Globs { set: b.build()?, rules: Rules { negated, any_positive } }))
}

/// `--archive-glob`: which files a directory walk keeps.
///
/// Split by shape, the way `.gitignore` reads a pattern: one without a `/`
/// matches the file name at any depth (`2026-08-*.tgz`), one with a `/`
/// matches the path relative to the directory that was named (`host1/*.tgz`).
/// The two kinds stay one ordered list for [`Rules`], so the last match wins
/// across them: `!2026-09-*` then `web1/*` keeps web1's September.
pub struct ArchiveGlobs {
    names: globset::GlobSet,
    paths: globset::GlobSet,
    /// Position on the command line of each glob in `names` and `paths`.
    name_at: Vec<usize>,
    path_at: Vec<usize>,
    rules: Rules,
}

impl ArchiveGlobs {
    fn is_match(&self, root: &Path, file: &Path) -> bool {
        let by_name = file.file_name().map(|n| self.names.matches(n)).unwrap_or_default();
        let by_path = file.strip_prefix(root).map(|r| self.paths.matches(r)).unwrap_or_default();
        let last = by_name.into_iter().map(|i| self.name_at[i])
            .chain(by_path.into_iter().map(|i| self.path_at[i]))
            .max();
        self.rules.keep(last)
    }
}

pub fn build_archive_globs(pats: &[String]) -> anyhow::Result<Option<ArchiveGlobs>> {
    if pats.is_empty() {
        return Ok(None);
    }
    let mut names = globset::GlobSetBuilder::new();
    let mut paths = globset::GlobSetBuilder::new();
    let (mut name_at, mut path_at, mut negated) = (Vec::new(), Vec::new(), Vec::new());
    for (i, p) in pats.iter().enumerate() {
        let (neg, glob) = parse_rule(p);
        let g = globset::Glob::new(glob)?;
        if glob.contains('/') {
            paths.add(g);
            path_at.push(i);
        } else {
            names.add(g);
            name_at.push(i);
        }
        negated.push(neg);
    }
    let any_positive = negated.contains(&false);
    Ok(Some(ArchiveGlobs {
        names: names.build()?,
        paths: paths.build()?,
        name_at,
        path_at,
        rules: Rules { negated, any_positive },
    }))
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
                if e.file_type().is_some_and(|t| t.is_file()) {
                    out.push(e.into_path());
                }
            }
            Err(e) => errors.push(format!("{}: {e}", root.display())),
        }
    }
    out
}


