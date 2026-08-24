//! `-g` must filter at the finest available granularity: tar member names
//! for archives (already covered by `tests/members.rs`), and the file's own
//! path for plain (non-tar) files. Before this, `-g` was silently ignored
//! for plain files, so a mixed sweep of live logs + archives searched every
//! live log regardless of `-g`.

mod helpers;

use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_trg")).args(args).output().unwrap()
}

#[test]
fn plain_file_matching_glob_is_searched() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("x.access.log");
    std::fs::write(&p, b"NEEDLE plain\n").unwrap();

    let out = run(&["-g", "*.access.log", "NEEDLE", p.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("NEEDLE plain"));
}

#[test]
fn plain_file_not_matching_glob_is_not_searched() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("x.error.log");
    std::fs::write(&p, b"NEEDLE plain\n").unwrap();

    let out = run(&["-g", "*.access.log", "NEEDLE", p.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1), "expected no-match exit code, got: {out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).is_empty());
}

#[test]
fn plain_files_still_searched_with_no_glob_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("x.error.log");
    std::fs::write(&p, b"NEEDLE plain\n").unwrap();

    let out = run(&["NEEDLE", p.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("NEEDLE plain"));
}

#[test]
fn archive_member_globbing_is_unaffected() {
    let f = helpers::tgz("g.tgz", &[
        ("logs/a.access.log", b"NEEDLE access\n"),
        ("logs/a.error.log", b"NEEDLE error\n"),
    ]);

    let out = run(&["-g", "*.access.log", "NEEDLE", f.path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("a.access.log"), "got {stdout:?}");
    assert!(!stdout.contains("a.error.log"), "got {stdout:?}");
}
