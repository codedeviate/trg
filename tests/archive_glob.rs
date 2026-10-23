mod helpers;

use std::path::Path;
use std::process::Command;

fn run(args: &[&str]) -> (String, String, Option<i32>) {
    let o = Command::new(env!("CARGO_BIN_EXE_trg")).args(args).output().unwrap();
    (String::from_utf8_lossy(&o.stdout).into_owned(),
     String::from_utf8_lossy(&o.stderr).into_owned(),
     o.status.code())
}

/// A directory of daily archives, each holding one line naming its own day,
/// so the output shows exactly which archives were searched.
fn archive_dir(names: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for name in names {
        let body = format!("NEEDLE from {name}\n");
        let f = helpers::tgz("x.tgz", &[("logs/a.log", body.as_bytes())]);
        let dest = dir.path().join(name);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::copy(&f.path, &dest).unwrap();
    }
    dir
}

fn p(d: &Path) -> &str {
    d.to_str().unwrap()
}

#[test]
fn a_walk_searches_only_archives_matching_the_glob() {
    let d = archive_dir(&["2026-08-01.tgz", "2026-08-02.tgz", "2026-09-01.tgz"]);
    let (out, err, c) = run(&["--archive-glob", "2026-08-*.tgz", "NEEDLE", p(d.path())]);
    assert!(out.contains("from 2026-08-01.tgz") && out.contains("from 2026-08-02.tgz"), "got: {out} / {err}");
    assert!(!out.contains("2026-09-01"), "a non-matching archive was searched: {out}");
    assert_eq!(c, Some(0));
}

/// No `/` in the glob: it matches the file name, however deep the walk goes.
#[test]
fn a_name_glob_matches_at_any_depth() {
    let d = archive_dir(&["host1/2026-08-01.tgz", "host2/deep/2026-08-01.tgz", "host2/2026-09-01.tgz"]);
    let (out, _, c) = run(&["--archive-glob", "2026-08-*", "NEEDLE", p(d.path())]);
    assert!(out.contains("from host1/2026-08-01.tgz"), "got: {out}");
    assert!(out.contains("from host2/deep/2026-08-01.tgz"), "got: {out}");
    assert!(!out.contains("2026-09-01"), "got: {out}");
    assert_eq!(c, Some(0));
}

/// A `/` in the glob: it matches the path relative to the directory named.
#[test]
fn a_path_glob_matches_relative_to_the_walk_root() {
    let d = archive_dir(&["host1/2026-08-01.tgz", "host2/2026-08-01.tgz"]);
    let (out, _, c) = run(&["--archive-glob", "host1/*.tgz", "NEEDLE", p(d.path())]);
    assert!(out.contains("from host1/2026-08-01.tgz"), "got: {out}");
    assert!(!out.contains("from host2/"), "got: {out}");
    assert_eq!(c, Some(0));
}

#[test]
fn several_globs_select_the_union() {
    let d = archive_dir(&["a.tgz", "b.tar.zst", "c.txt"]);
    let (out, _, _) = run(&["--archive-glob", "*.tgz", "--archive-glob", "*.tar.zst", "NEEDLE", p(d.path())]);
    assert!(out.contains("from a.tgz") && out.contains("from b.tar.zst"), "got: {out}");
    assert!(!out.contains("from c.txt"), "got: {out}");
}

/// Like `rg -g`: naming a file is a stronger statement than any filter.
#[test]
fn an_explicitly_named_archive_is_searched_regardless() {
    let d = archive_dir(&["2026-08-01.tgz", "2026-09-01.tgz"]);
    let named = d.path().join("2026-09-01.tgz");
    let (out, _, c) = run(&["--archive-glob", "2026-08-*", "NEEDLE", p(d.path()), p(&named)]);
    assert!(out.contains("from 2026-09-01.tgz"), "an explicitly named archive was filtered: {out}");
    assert_eq!(c, Some(0));
}

/// A typo'd glob must never look like clean logs: exit 1 is the claim that
/// everything asked for was read and nothing matched.
#[test]
fn a_glob_matching_nothing_is_two_not_one() {
    let d = archive_dir(&["2026-08-01.tgz"]);
    let (out, err, c) = run(&["--archive-glob", "2026-8-*", "NEEDLE", p(d.path())]);
    assert_eq!(c, Some(2), "a glob that selected nothing must not exit 1");
    assert!(err.contains("--archive-glob matched no files"), "must say why on stderr: {err}");
    assert!(out.is_empty(), "got: {out}");
}

/// The check is across the run, not per directory: one directory without a
/// match is normal when another has them.
#[test]
fn one_directory_without_matches_is_fine_when_another_has_them() {
    let a = archive_dir(&["2026-08-01.tgz"]);
    let b = archive_dir(&["2026-09-01.tgz"]);
    let (out, err, c) = run(&["--archive-glob", "2026-08-*", "NEEDLE", p(a.path()), p(b.path())]);
    assert!(out.contains("from 2026-08-01.tgz"), "got: {out}");
    assert!(err.is_empty(), "no warning expected: {err}");
    assert_eq!(c, Some(0));
}

#[test]
fn an_invalid_archive_glob_is_two() {
    let d = archive_dir(&["a.tgz"]);
    let (_, err, c) = run(&["--archive-glob", "a[", "NEEDLE", p(d.path())]);
    assert_eq!(c, Some(2));
    assert!(err.contains("a["), "the error must name the bad glob: {err}");
    assert!(!err.contains("invalid option"), "the flag itself was not recognised: {err}");
}

#[test]
fn help_lists_archive_glob() {
    let (out, _, _) = run(&["--help"]);
    assert!(out.contains("--archive-glob"), "help is missing --archive-glob");
}
