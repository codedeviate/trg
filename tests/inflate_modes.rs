mod helpers;

use std::process::Command;

fn run(mode: &str, path: &std::path::Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_trg"))
        .args(["--inflate", mode, "NEEDLE", path.to_str().unwrap()])
        .output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn all_three_modes_produce_identical_output() {
    let big = vec![b'x'; 400_000];
    let f = helpers::tgz("m.tgz", &[
        ("logs/a.log", b"NEEDLE one\n"),
        ("logs/pad.log", &big),
        ("logs/b.log", b"NEEDLE two\n"),
    ]);
    let auto = run("auto", &f.path);
    let stream = run("stream", &f.path);
    let buffer = run("buffer", &f.path);
    assert_eq!(auto, stream, "auto and stream disagreed");
    assert_eq!(auto, buffer, "auto and buffer disagreed");
    assert!(auto.contains("NEEDLE one") && auto.contains("NEEDLE two"));
}

#[test]
fn a_tiny_budget_forces_streaming_without_changing_output() {
    let f = helpers::tgz("s.tgz", &[("logs/a.log", b"NEEDLE\n")]);
    let a = run("auto", &f.path);
    let out = Command::new(env!("CARGO_BIN_EXE_trg"))
        .args(["--inflate-budget", "1", "NEEDLE", f.path.to_str().unwrap()])
        .output().unwrap();
    assert_eq!(a, String::from_utf8_lossy(&out.stdout));
}
