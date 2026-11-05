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

    // Beside a file `-g` keeps, so the run is a clean 1 rather than the 2 of
    // a `-g` that kept nothing at all.
    let q = dir.path().join("x.access.log");
    std::fs::write(&q, b"nothing\n").unwrap();
    let out = run(&["-g", "*.access.log", "NEEDLE", p.to_str().unwrap(), q.to_str().unwrap()]);
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

/// A file reached by two names was searched once, under the first, and `-g`
/// was then judged against that name alone. `-g '*current.log'` on a
/// directory holding `access.log.2026-11-05` and a symlink or hardlink
/// `current.log` to it searched nothing and exited 1.
#[cfg(unix)]
#[test]
fn glob_picks_a_file_by_any_name_it_was_reached_by() {
    for link in ["symlink", "hardlink"] {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("access.log.2026-11-05");
        std::fs::write(&real, b"ERR here\n").unwrap();
        let alias = dir.path().join("current.log");
        match link {
            "symlink" => std::os::unix::fs::symlink("access.log.2026-11-05", &alias).unwrap(),
            _ => std::fs::hard_link(&real, &alias).unwrap(),
        }
        std::fs::write(dir.path().join("other-current.log"), b"fine\n").unwrap();

        let out = run(&["-g", "*current.log", "ERR", dir.path().to_str().unwrap()]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.code(), Some(0), "{link}: {out:?}");
        assert_eq!(stdout.trim(), format!("{}:1:ERR here", alias.display()), "{link}");

        // Without -g it is still searched once, under the first name.
        let out = run(&["-c", "ERR", dir.path().to_str().unwrap()]);
        assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 1, "{link}: {out:?}");
    }
}

/// `-g` judges a plain file's path relative to the current directory, as rg
/// does, so `logs`, `./logs` and an absolute path select the same files.
#[test]
fn glob_on_plain_files_ignores_how_the_path_was_spelled() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("logs")).unwrap();
    std::fs::write(dir.path().join("logs/b.log"), b"ERR plain\n").unwrap();
    let abs = dir.path().join("logs");
    for p in ["logs", "./logs", abs.to_str().unwrap()] {
        let out = Command::new(env!("CARGO_BIN_EXE_trg"))
            .current_dir(dir.path())
            .args(["-g", "logs/*.log", "-c", "ERR", p])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{p}: {out:?}");
    }
}
