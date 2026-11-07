//! Positional arguments become an ordered list of archives to search.
//! Order is preserved because these archives are days, so argument order is
//! chronological order.

use std::collections::HashMap;
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
///
/// A dropped occurrence is kept as an alias of the path that was searched:
/// `-g` picks plain files by name, and the name it picks may be the one that
/// was dropped (`current.log`, a symlink to `access.log.2026-11-05`).
fn push_unique(
    items: &mut Vec<PathBuf>,
    seen: &mut HashMap<Identity, usize>,
    aliases: &mut Aliases,
    p: PathBuf,
) {
    match identity(&p) {
        Some(id) => match seen.get(&id) {
            Some(&i) => aliases.entry(items[i].clone()).or_default().push(p),
            None => {
                seen.insert(id, items.len());
                items.push(p);
            }
        },
        None => items.push(p),
    }
}

/// The other names a searched file was reached by, keyed by the name it was
/// searched under.
pub type Aliases = HashMap<PathBuf, Vec<PathBuf>>;

/// Everything `resolve_all` found: the files to search, in order, the paths
/// that failed, and the [`Aliases`].
pub struct Resolved {
    pub items: Vec<PathBuf>,
    pub errors: Vec<String>,
    pub aliases: Aliases,
}

pub fn resolve(paths: &[PathBuf]) -> (Vec<PathBuf>, Vec<String>) {
    resolve_with(paths, None)
}

pub fn resolve_with(
    paths: &[PathBuf],
    archive_globs: Option<&ArchiveGlobs>,
) -> (Vec<PathBuf>, Vec<String>) {
    let r = resolve_all(paths, archive_globs);
    (r.items, r.errors)
}

/// `-` names standard input, as in rg and grep: `zcat x.gz | trg ERR -`.
pub fn is_stdin(p: &Path) -> bool {
    p.as_os_str() == "-"
}

/// A path as trg names it: `<stdin>` for `-`, as rg does.
pub fn display(p: &Path) -> String {
    if is_stdin(p) { "<stdin>".to_string() } else { p.display().to_string() }
}

/// The path `-g` judges a plain file by: relative to the current directory,
/// as rg's are, so `logs`, `./logs` and `$PWD/logs` select the same files.
/// Spelled as given, `-g 'logs/*.log' ./logs` kept nothing while `logs`
/// kept everything.
///
/// The current directory comes back resolved (`/private/var/...` on macOS,
/// where `/var` is a symlink), so an absolute path that does not start with
/// it is tried again with its directory resolved. Only the directory: the
/// file's own name is what `-g` asked about, symlink or not.
pub fn glob_candidate(p: &Path) -> PathBuf {
    let rel = match std::env::current_dir() {
        Ok(cwd) if p.is_absolute() => match p.strip_prefix(&cwd) {
            Ok(r) => r.to_path_buf(),
            Err(_) => p
                .parent()
                .and_then(|d| std::fs::canonicalize(d).ok())
                .and_then(|d| Some(d.strip_prefix(&cwd).ok()?.join(p.file_name()?)))
                .unwrap_or_else(|| p.to_path_buf()),
        },
        _ => p.to_path_buf(),
    };
    rel.components().filter(|c| !matches!(c, std::path::Component::CurDir)).collect()
}

/// `resolve`, keeping only the walked files that `archive_globs` selects, and
/// the other names each file was reached by.
///
/// A named directory that holds no files at all is an error (see below for
/// one whose files the globs all left out).
///
/// Files named on the command line are never filtered, as with `rg -g`: naming
/// a file is a stronger statement than any pattern.
///
/// A glob that selects nothing anywhere in the walk is an error, not an empty
/// search: the run would otherwise exit 1, the claim that everything asked for
/// was read and nothing matched, and a typo like `2026-8-*` would read as
/// clean logs. It is judged across the whole run, so one directory without a
/// match is normal when another has them.
pub fn resolve_all(paths: &[PathBuf], archive_globs: Option<&ArchiveGlobs>) -> Resolved {
    let mut items = Vec::new();
    let mut errors = Vec::new();
    let mut seen = HashMap::new();
    let mut aliases = Aliases::new();
    let mut walked_any = false;
    let mut kept_any = false;

    for p in paths {
        // Read once: a second `-` would find it already drained, and a
        // pipe's identity says nothing about what it carries.
        if is_stdin(p) {
            if !items.iter().any(|i: &PathBuf| is_stdin(i)) {
                items.push(p.clone());
            }
            continue;
        }
        match std::fs::metadata(p) {
            Err(e) => errors.push(format!("{}: {e}", p.display())),
            Ok(md) if md.is_dir() => {
                let before = errors.len();
                let mut found = walk(p, &mut errors);
                // A directory with no files in it is a 2, not a clean 1: it is
                // a mount point with nothing mounted, a log directory spelled
                // wrong by a day, a rotation that has not run. rg says 1, and
                // a 1 says the logs were read and are clean. Not when the
                // walk already failed: that error is the whole story.
                if found.is_empty() && errors.len() == before {
                    errors.push(format!("{}: no files in this directory", p.display()));
                }
                // Only a directory with files in it can say anything about
                // the globs: an empty one has its own error above.
                walked_any |= !found.is_empty();
                if let Some(g) = archive_globs {
                    found.retain(|f| g.is_match(p, f));
                }
                kept_any |= !found.is_empty();
                found.sort();
                for f in found {
                    push_unique(&mut items, &mut seen, &mut aliases, f);
                }
            }
            Ok(_) => push_unique(&mut items, &mut seen, &mut aliases, p.clone()),
        }
    }

    if archive_globs.is_some() && walked_any && !kept_any {
        errors.push("--archive-glob matched no files in the directories searched".to_string());
    }

    Resolved { items, errors, aliases }
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
///
/// Every glob matches against the member's whole path; one without a `/` also
/// matches its file name, so `-g access.log` finds `logs/access.log` as it
/// would in ripgrep. Strictly a superset of whole-path matching, which was all
/// there was before 0.7.0, so no glob selects fewer members than it used to.
pub struct Globs {
    paths: globset::GlobSet,
    names: globset::GlobSet,
    /// Position on the command line of each glob in `names`.
    name_at: Vec<usize>,
    rules: Rules,
}

impl Globs {
    pub fn is_match(&self, candidate: impl AsRef<Path>) -> bool {
        // `tar -czf day.tgz -C /var/log .` names its members `./apache2/...`,
        // and `-g 'apache2/*'` matched none of them: beside an archive named
        // without the `./`, the run read the one and skipped the other. The
        // leading `.` is how the archive was made, not where the file is.
        let mut candidate = candidate.as_ref();
        while let Ok(rest) = candidate.strip_prefix(".") {
            candidate = rest;
        }
        let by_name = candidate.file_name().map(|n| self.names.matches(n)).unwrap_or_default();
        let last = self.paths.matches(candidate).into_iter()
            .chain(by_name.into_iter().map(|i| self.name_at[i]))
            .max();
        self.rules.keep(last)
    }
}

pub fn build_globs(pats: &[String]) -> anyhow::Result<Option<Globs>> {
    if pats.is_empty() {
        return Ok(None);
    }
    let mut paths = globset::GlobSetBuilder::new();
    let mut names = globset::GlobSetBuilder::new();
    let (mut name_at, mut negated) = (Vec::new(), Vec::new());
    for (i, p) in pats.iter().enumerate() {
        let (neg, glob) = parse_rule(p);
        let g = globset::Glob::new(glob)?;
        if !glob.contains('/') {
            names.add(g.clone());
            name_at.push(i);
        }
        paths.add(g);
        negated.push(neg);
    }
    let any_positive = negated.contains(&false);
    Ok(Some(Globs {
        paths: paths.build()?,
        names: names.build()?,
        name_at,
        rules: Rules { negated, any_positive },
    }))
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
            Err(e) => errors.push(walk_error(root, &e)),
        }
    }
    out
}

/// A walk error, naming the path it is about once. `ignore`'s own text
/// already holds the path, and twice over for an I/O error ("IO error for
/// operation on x: ..."), so prefixed with the root it read
/// `t1: t1/locked: IO error for operation on t1/locked: Permission denied`.
fn walk_error(root: &Path, e: &ignore::Error) -> String {
    // The path the error is about, if `ignore` attached one, and the text
    // without it. An I/O error's own text is walkdir's, which names the path
    // again, so it is rebuilt from the OS error behind it.
    fn inner(e: &ignore::Error) -> (Option<&Path>, String) {
        match e {
            ignore::Error::WithPath { path, err } => (Some(path), inner(err).1),
            ignore::Error::WithDepth { err, .. } | ignore::Error::WithLineNumber { err, .. } => inner(err),
            ignore::Error::Io(io) => {
                let cause = io.get_ref().and_then(std::error::Error::source);
                (None, cause.map_or_else(|| io.to_string(), ToString::to_string))
            }
            // A loop names both of its paths in its own text.
            ignore::Error::Loop { child, .. } => (Some(child), "a file system loop: it points to a directory above it".into()),
            other => (None, other.to_string()),
        }
    }
    match inner(e) {
        (Some(path), text) => format!("{}: {text}", path.display()),
        (None, text) => format!("{}: {text}", root.display()),
    }
}


