mod helpers;

use std::process::Command;

/// The primary oracle. Extract every member, run `rg` on each one
/// individually, and require trg's output to agree member-for-member and
/// line-for-line. This is what validates "every member is its own stream".
fn rg_ground_truth(dir: &std::path::Path, pattern: &str) -> Vec<String> {
    let out = Command::new("rg")
        .args(["--no-heading", "-n", "--color", "never", "--sort", "path", pattern, "."])
        .current_dir(dir)
        .output()
        .expect("rg must be installed to run the differential oracle");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim_start_matches("./").to_string())
        .collect()
}

fn body(members: &[(&str, &[u8])], pattern: &str) {
    let f = helpers::tgz("d.tgz", members);

    // ground truth: extract, then rg each member
    let ex = tempfile::tempdir().unwrap();
    let status = Command::new("tar")
        .args(["xzf", f.path.to_str().unwrap(), "-C", ex.path().to_str().unwrap()])
        .status().unwrap();
    assert!(status.success());
    let mut want = rg_ground_truth(ex.path(), pattern);

    // trg, with the archive prefix stripped so the two are comparable
    let out = Command::new(env!("CARGO_BIN_EXE_trg"))
        .args([pattern, f.path.to_str().unwrap()])
        .output().unwrap();
    let prefix = format!("{}:", f.path.display());
    let mut got: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.strip_prefix(&prefix).unwrap_or(l).to_string())
        .collect();

    want.sort();
    got.sort();
    assert_eq!(got, want, "trg disagreed with rg-on-extracted-members");
}

#[test]
fn agrees_with_rg_on_line_numbers() {
    body(&[
        ("logs/a.log", b"one\ntwo NEEDLE\nthree\nfour NEEDLE\n"),
        ("logs/b.log", b"NEEDLE first line\nnope\n"),
    ], "NEEDLE");
}

#[test]
fn agrees_when_a_member_has_no_trailing_newline() {
    body(&[("logs/a.log", b"alpha\nNEEDLE last line no newline")], "NEEDLE");
}

#[test]
fn agrees_on_crlf_members() {
    body(&[("logs/a.log", b"one\r\nNEEDLE two\r\nthree\r\n")], "NEEDLE");
}

#[test]
fn agrees_on_a_zero_byte_member() {
    body(&[("logs/empty.log", b""), ("logs/a.log", b"NEEDLE\n")], "NEEDLE");
}

#[test]
fn matches_lines_containing_invalid_utf8() {
    // scanner traffic puts arbitrary bytes in request paths
    let mut line = b"GET /NEEDLE?q=".to_vec();
    line.extend_from_slice(&[0xff, 0xfe, 0x80]);
    line.push(b'\n');
    body(&[("logs/a.log", &line)], "NEEDLE");
}

#[test]
fn does_not_need_dash_a_for_text_members() {
    let f = helpers::tgz("t.tgz", &[("logs/a.log", b"NEEDLE plain text\n")]);
    let out = Command::new(env!("CARGO_BIN_EXE_trg"))
        .args(["NEEDLE", f.path.to_str().unwrap()])
        .output().unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("logs/a.log:1:"), "expected a match without -a, got: {s}");
    assert!(!s.contains("binary"), "text member misdetected as binary: {s}");
}

#[test]
fn output_path_is_archive_then_member() {
    let f = helpers::tgz("p.tgz", &[("logs/vhost03.access.log", b"NEEDLE\n")]);
    let out = Command::new(env!("CARGO_BIN_EXE_trg"))
        .args(["NEEDLE", f.path.to_str().unwrap()])
        .output().unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    let want = format!("{}:logs/vhost03.access.log:1:NEEDLE", f.path.display());
    assert!(s.contains(&want), "got: {s}");
}
