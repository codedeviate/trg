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

// --- Fix round 1: --inflate-budget is a hard ceiling in every mode ---

fn run_with_budget(mode: &str, budget: &str, path: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_trg"))
        .args(["--inflate", mode, "--inflate-budget", budget, "NEEDLE", path.to_str().unwrap()])
        .output().unwrap()
}

/// `--inflate=buffer` explicitly requested, but the archive decompresses far
/// past the budget: `open_decoded_with`'s fallback must produce output
/// byte-identical to streaming, not merely "not crash".
#[test]
fn buffer_mode_falls_back_to_streaming_output_when_the_archive_exceeds_the_budget() {
    // Highly compressible on disk (so the *compressed file* stays under the
    // budget and it is the *decompressed* side that blows it), but large
    // enough once untarred that a 1M capacity/ceiling cannot hold it.
    let big = vec![b'x'; 2_000_000];
    let f = helpers::tgz("big.tgz", &[
        ("logs/a.log", b"NEEDLE one\n"),
        ("logs/pad.log", &big),
        ("logs/b.log", b"NEEDLE two\n"),
    ]);
    let stream = run_with_budget("stream", "1M", &f.path);
    let buffer = run_with_budget("buffer", "1M", &f.path);
    assert_eq!(stream.status.code(), buffer.status.code());
    assert_eq!(
        String::from_utf8_lossy(&stream.stdout),
        String::from_utf8_lossy(&buffer.stdout),
        "buffer mode with an undersized budget must fall back to streaming's output, not diverge"
    );
    let out = String::from_utf8_lossy(&buffer.stdout);
    assert!(out.contains("NEEDLE one") && out.contains("NEEDLE two"));
}

/// `--inflate-budget 0` must still behave like "never buffer-inflate" end to
/// end — not a parse error, and not silently ignored by `buffer` mode.
#[test]
fn a_zero_budget_is_not_an_error_and_still_forces_streaming_output() {
    let f = helpers::tgz("zero.tgz", &[("logs/a.log", b"NEEDLE\n")]);
    let stream = run_with_budget("stream", "0", &f.path);
    let auto = run_with_budget("auto", "0", &f.path);
    let buffer = run_with_budget("buffer", "0", &f.path);
    // Exit code 2 means "something went unread" (a parse failure or a search
    // error) — a bare `--inflate-budget 0` must not produce that.
    assert_ne!(stream.status.code(), Some(2), "stream: {:?}", stream);
    assert_ne!(auto.status.code(), Some(2), "auto: {:?}", auto);
    assert_ne!(buffer.status.code(), Some(2), "buffer: {:?}", buffer);
    assert_eq!(
        String::from_utf8_lossy(&stream.stdout),
        String::from_utf8_lossy(&auto.stdout)
    );
    assert_eq!(
        String::from_utf8_lossy(&stream.stdout),
        String::from_utf8_lossy(&buffer.stdout),
        "buffer mode explicitly requested with budget 0 must still behave like streaming"
    );
}
