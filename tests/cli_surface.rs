use std::process::Command;

#[test]
fn help_lists_the_archive_flags() {
    let out = Command::new(env!("CARGO_BIN_EXE_trg")).arg("--help").output().unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    for flag in ["--glob", "--list-members", "--archive-sep", "--inflate", "--turbo", "--escape"] {
        assert!(s.contains(flag), "help is missing {flag}");
    }
}

#[test]
fn no_pattern_is_an_error() {
    let out = Command::new(env!("CARGO_BIN_EXE_trg")).output().unwrap();
    assert_ne!(out.status.code(), Some(0));
}

/// The help text is printed to a terminal like everything else.
#[test]
fn help_contains_no_control_bytes() {
    let out = Command::new(env!("CARGO_BIN_EXE_trg")).arg("--help").output().unwrap();
    let bad: Vec<u8> = out.stdout.iter().copied()
        .filter(|&b| b < 0x20 && b != b'\n').collect();
    assert!(bad.is_empty(), "control bytes in --help: {bad:?}");
}
