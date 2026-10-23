mod helpers;

use std::process::Command;

fn run(args: &[&str]) -> (String, String, Option<i32>) {
    let o = Command::new(env!("CARGO_BIN_EXE_trg")).args(args).output().unwrap();
    (String::from_utf8_lossy(&o.stdout).into_owned(),
     String::from_utf8_lossy(&o.stderr).into_owned(),
     o.status.code())
}

/// Every member says its own name, so the output shows what was searched.
fn members() -> helpers::Fixture {
    helpers::tgz("m.tgz", &[
        ("logs/a.log", b"NEEDLE in a.log\n"),
        ("logs/b.log", b"NEEDLE in b.log\n"),
        ("logs/web.error.log", b"NEEDLE in web.error.log\n"),
        ("logs/notes.txt", b"NEEDLE in notes.txt\n"),
    ])
}

#[test]
fn a_negated_member_glob_excludes_and_keeps_the_rest() {
    let f = members();
    let (out, err, c) = run(&["-g", "!*.error.log", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(!out.contains("in web.error.log"), "excluded member was searched: {out}");
    for m in ["a.log", "b.log", "notes.txt"] {
        assert!(out.contains(&format!("in {m}")), "{m} should still be searched: {out} / {err}");
    }
    assert_eq!(c, Some(0));
}

#[test]
fn a_whitelist_and_an_exclusion_combine() {
    let f = members();
    let (out, _, _) = run(&["-g", "*.log", "-g", "!*.error.log", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("in a.log") && out.contains("in b.log"), "got: {out}");
    assert!(!out.contains("in web.error.log"), "got: {out}");
    assert!(!out.contains("in notes.txt"), "not whitelisted, still searched: {out}");
}

/// ripgrep's rule, and gitignore's: when several globs match, the last one
/// given decides.
#[test]
fn the_last_matching_glob_wins() {
    let f = members();
    let (out, _, _) = run(&["-g", "!*.log", "-g", "logs/a.log", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("in a.log"), "a later whitelist must override an earlier exclusion: {out}");
    assert!(!out.contains("in b.log"), "got: {out}");
    // A whitelist exists, so a member matching nothing is not searched.
    assert!(!out.contains("in notes.txt"), "got: {out}");

    let (out, _, _) = run(&["-g", "logs/a.log", "-g", "!*.log", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(!out.contains("in a.log"), "a later exclusion must override an earlier whitelist: {out}");
}

/// Exclusion is how `-g` reads for people coming from rg. A bare `!` glob was
/// silently treated as a literal before, matching nothing.
#[test]
fn only_exclusions_means_everything_else_is_searched() {
    let f = members();
    let (out, _, _) = run(&["-g", "!logs/a.log", "-g", "!logs/b.log", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("in web.error.log") && out.contains("in notes.txt"), "got: {out}");
    assert!(!out.contains("in a.log") && !out.contains("in b.log"), "got: {out}");
}

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

#[test]
fn a_negated_archive_glob_excludes_archives() {
    let d = archive_dir(&["2026-08-01.tgz", "2026-09-01.tgz", "old/2026-09-02.tgz"]);
    let (out, err, c) = run(&["--archive-glob", "!2026-09-*", "NEEDLE", d.path().to_str().unwrap()]);
    assert!(out.contains("from 2026-08-01.tgz"), "got: {out} / {err}");
    assert!(!out.contains("2026-09-"), "excluded archive searched: {out}");
    assert_eq!(c, Some(0));
}

/// Name globs and path globs are one ordered list, so last-wins holds across
/// the two kinds.
#[test]
fn last_wins_across_name_and_path_archive_globs() {
    let d = archive_dir(&["web1/2026-09-01.tgz", "web2/2026-09-01.tgz"]);
    let (out, _, _) = run(&["--archive-glob", "!2026-09-*", "--archive-glob", "web1/*",
                            "NEEDLE", d.path().to_str().unwrap()]);
    assert!(out.contains("from web1/2026-09-01.tgz"), "got: {out}");
    assert!(!out.contains("from web2/"), "got: {out}");

    // Reversed, the exclusion comes last and wins over the path glob.
    let d = archive_dir(&["web1/2026-08-01.tgz", "web1/2026-09-01.tgz"]);
    let (out, _, _) = run(&["--archive-glob", "web1/*", "--archive-glob", "!2026-09-*",
                            "NEEDLE", d.path().to_str().unwrap()]);
    assert!(out.contains("from web1/2026-08-01.tgz"), "got: {out}");
    assert!(!out.contains("2026-09-01"), "a later name exclusion must beat an earlier path glob: {out}");
}

/// Excluding everything is still a glob that selected nothing.
#[test]
fn an_archive_glob_excluding_everything_is_two() {
    let d = archive_dir(&["a.tgz"]);
    let (_, err, c) = run(&["--archive-glob", "!*", "NEEDLE", d.path().to_str().unwrap()]);
    assert_eq!(c, Some(2));
    assert!(err.contains("--archive-glob matched no files"), "got: {err}");
}
