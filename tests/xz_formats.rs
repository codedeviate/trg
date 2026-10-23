mod helpers;

use std::process::Command;

fn run(args: &[&str]) -> (String, String, Option<i32>) {
    let o = Command::new(env!("CARGO_BIN_EXE_trg")).args(args).output().unwrap();
    (String::from_utf8_lossy(&o.stdout).into_owned(),
     String::from_utf8_lossy(&o.stderr).into_owned(),
     o.status.code())
}

/// The differential oracle, extended: rg -z supports xz, so it stays the
/// authority on per-member line numbers for .tar.xz too.
#[test]
fn agrees_with_rg_on_line_numbers_in_a_tar_xz() {
    let f = helpers::tar_xz("d.tar.xz", &[
        ("logs/a.log", b"one\ntwo NEEDLE\nthree\nfour NEEDLE\n"),
        ("logs/b.log", b"NEEDLE first line\nnope\n"),
    ]);
    // Extract in-process rather than shelling out: GNU tar delegates .tar.xz
    // to an `xz` binary that is not present everywhere, and a test that fails
    // on a missing external tool tells you nothing about trg.
    let ex = tempfile::tempdir().unwrap();
    let mut raw = Vec::new();
    std::io::Read::read_to_end(
        &mut liblzma::read::XzDecoder::new(std::fs::File::open(&f.path).unwrap()),
        &mut raw,
    ).unwrap();
    tar::Archive::new(&raw[..]).unpack(ex.path()).unwrap();

    let truth = Command::new("rg")
        .args(["--no-heading", "-n", "--color", "never", "--sort", "path", "NEEDLE", "."])
        .current_dir(ex.path()).output().unwrap();
    let mut want: Vec<String> = String::from_utf8_lossy(&truth.stdout)
        .lines().map(|l| l.trim_start_matches("./").to_string()).collect();

    let (out, _, _) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    let prefix = format!("{}:", f.path.display());
    let mut got: Vec<String> = out.lines()
        .map(|l| l.strip_prefix(&prefix).unwrap_or(l).to_string()).collect();
    want.sort(); got.sort();
    assert_eq!(got, want, "trg disagreed with rg on a .tar.xz");
}

#[test]
fn a_tar_xz_searches_like_a_tgz() {
    let f = helpers::tar_xz("a.tar.xz", &[("logs/x.log", b"one\nNEEDLE two\n")]);
    let (out, _, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("logs/x.log:2:NEEDLE two"), "got: {out}");
    assert_eq!(c, Some(0));
}

/// The trap: a nesting check naming only gzip would hand this to the searcher
/// as binary and silently find nothing.
#[test]
fn an_xz_member_inside_a_tar_xz_is_decompressed() {
    let f = helpers::tar_xz_nested_xz("n.tar.xz", "logs/access.log.1.xz", b"NEEDLE nested\n");
    let (out, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("NEEDLE nested"), "nested .xz not decompressed: {out} / {err}");
    assert_eq!(c, Some(0));
}

/// A host mid-migration: gzip outer, xz member.
#[test]
fn an_xz_member_inside_a_tgz_is_decompressed() {
    let f = helpers::tgz_nested_xz("m.tgz", "logs/access.log.1.xz", b"NEEDLE mixed\n");
    let (out, _, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("NEEDLE mixed"), "got: {out}");
    assert_eq!(c, Some(0));
}

#[test]
fn nesting_is_still_capped_at_one_level() {
    let inner = helpers::xz_bytes(b"NEEDLE deep\n");
    let f = helpers::tar_xz_nested_xz("d2.tar.xz", "logs/a.log.xz", &inner);
    let (out, _, _) = run(&["NEEDLE deep", f.path.to_str().unwrap()]);
    assert!(!out.contains("NEEDLE deep"), "recursed past one level; bomb guard gone: {out}");
}

#[test]
fn a_truncated_tar_xz_is_two_not_one() {
    let big = vec![b'x'; 400_000];
    let f = helpers::tar_xz_truncated("t.tar.xz", &[("logs/a.log", &big)], 0.4);
    let (_, err, c) = run(&["NEEDLE-absent", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(2), "an unreadable .tar.xz must never look like a clean 1");
    assert!(!err.is_empty(), "must diagnose on stderr");
}

#[test]
fn all_inflate_modes_agree_on_a_tar_xz() {
    let f = helpers::tar_xz("i.tar.xz", &[("logs/a.log", b"NEEDLE one\n")]);
    let p = f.path.to_str().unwrap();
    let a = run(&["--inflate", "auto", "NEEDLE", p]).0;
    let s = run(&["--inflate", "stream", "NEEDLE", p]).0;
    let b = run(&["--inflate", "buffer", "NEEDLE", p]).0;
    assert_eq!(a, s, "auto and stream disagreed on xz");
    assert_eq!(a, b, "buffer did not fall back cleanly on xz");
    assert!(a.contains("NEEDLE one"));
}

#[test]
fn a_binary_member_in_a_tar_xz_is_still_skipped_without_dash_a() {
    let mut bin = b"NEEDLE".to_vec();
    bin.extend_from_slice(&[0u8, 1, 2, 0, 0]);
    bin.push(b'\n');
    let f = helpers::tar_xz("b.tar.xz", &[("logs/blob.bin", &bin)]);
    let (out, _, _) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(!out.contains("blob.bin:1:"), "binary member should be skipped: {out}");
    let (out_a, _, c) = run(&["-a", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(out_a.contains("blob.bin"), "-a must force it: {out_a}");
    assert_eq!(c, Some(0));
}

/// `cat a.xz b.xz` is a valid .xz file that `xz -d` decodes whole. A decoder
/// that stops after the first stream would silently drop the second half.
#[test]
fn concatenated_xz_streams_in_a_member_are_all_searched() {
    let mut two = helpers::xz_bytes(b"NEEDLE first stream\n");
    two.extend_from_slice(&helpers::xz_bytes(b"NEEDLE second stream\n"));
    let f = helpers::tar_xz("c.tar.xz", &[("logs/access.log.1.xz", two.as_slice())]);
    let (out, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("NEEDLE first stream"), "got: {out} / {err}");
    assert!(out.contains("NEEDLE second stream"), "second stream dropped: {out} / {err}");
    assert_eq!(c, Some(0));
}
