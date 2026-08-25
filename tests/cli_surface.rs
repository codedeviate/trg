use std::process::Command;

#[test]
fn help_lists_the_archive_flags() {
    let out = Command::new(env!("CARGO_BIN_EXE_trg")).arg("--help").output().unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    for flag in ["--glob", "--list-members", "--archive-sep", "--inflate", "--turbo"] {
        assert!(s.contains(flag), "help is missing {flag}");
    }
}

#[test]
fn no_pattern_is_an_error() {
    let out = Command::new(env!("CARGO_BIN_EXE_trg")).output().unwrap();
    assert_ne!(out.status.code(), Some(0));
}
