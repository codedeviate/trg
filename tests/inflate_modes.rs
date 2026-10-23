//! `--inflate` and `--inflate-budget` since 0.7.0: the whole-buffer path is
//! gone and trg always streams, but the flags are still accepted so scripts
//! that pass them keep working. Each one costs a deprecation note on stderr and
//! nothing else — not the output, and not the exit code.

mod helpers;

use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_trg")).args(args).output().unwrap()
}

fn fixture() -> helpers::Fixture {
    let big = vec![b'x'; 400_000];
    helpers::tgz("m.tgz", &[
        ("logs/a.log", b"NEEDLE one\n"),
        ("logs/pad.log", &big),
        ("logs/b.log", b"NEEDLE two\n"),
    ])
}

#[test]
fn the_old_flags_change_nothing_but_stderr() {
    let f = fixture();
    let p = f.path.to_str().unwrap();
    let plain = run(&["NEEDLE", p]);
    assert!(String::from_utf8_lossy(&plain.stdout).contains("NEEDLE two"));

    for flags in [
        &["--inflate", "buffer"][..],
        &["--inflate", "stream"],
        &["--inflate", "auto"],
        &["--inflate-budget", "1"],
        &["--inflate=buffer", "--inflate-budget", "2G"],
    ] {
        let mut args = flags.to_vec();
        args.extend(["NEEDLE", p]);
        let o = run(&args);
        assert_eq!(o.stdout, plain.stdout, "{flags:?} changed the output");
        assert_eq!(o.status.code(), Some(0), "{flags:?} changed the exit code");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(err.contains("deprecated"), "{flags:?}: no deprecation note: {err}");
        assert!(!err.contains("fell back"), "{flags:?}: a buffered attempt was made: {err}");
    }
}

#[test]
fn without_the_old_flags_there_is_no_note() {
    let f = fixture();
    let o = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(o.stderr.is_empty(), "got: {}", String::from_utf8_lossy(&o.stderr));
}

/// One note per flag, not one per archive.
#[test]
fn the_note_is_printed_once_however_many_archives() {
    let a = fixture();
    let b = fixture();
    let o = run(&["--inflate", "buffer", "NEEDLE", a.path.to_str().unwrap(), b.path.to_str().unwrap()]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(err.matches("deprecated").count(), 1, "got: {err}");
}
