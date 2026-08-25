mod helpers;

use std::process::Command;

fn run(args: &[&str]) -> (String, String, Option<i32>) {
    let o = Command::new(env!("CARGO_BIN_EXE_trg")).args(args).output().unwrap();
    (String::from_utf8_lossy(&o.stdout).into_owned(),
     String::from_utf8_lossy(&o.stderr).into_owned(),
     o.status.code())
}

/// The differential oracle, extended: rg -z supports zstd, so it stays the
/// authority on per-member line numbers for .tar.zst too.
#[test]
fn agrees_with_rg_on_line_numbers_in_a_tar_zst() {
    let f = helpers::tar_zst("d.tar.zst", &[
        ("logs/a.log", b"one\ntwo NEEDLE\nthree\nfour NEEDLE\n"),
        ("logs/b.log", b"NEEDLE first line\nnope\n"),
    ]);
    // Extract in-process rather than shelling out: GNU tar delegates .tar.zst
    // to a `zstd` binary that is not present everywhere, and a test that fails
    // on a missing external tool tells you nothing about trg.
    let ex = tempfile::tempdir().unwrap();
    let raw = zstd::stream::decode_all(std::fs::File::open(&f.path).unwrap()).unwrap();
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
    assert_eq!(got, want, "trg disagreed with rg on a .tar.zst");
}

#[test]
fn a_tar_zst_searches_like_a_tgz() {
    let f = helpers::tar_zst("a.tar.zst", &[("logs/x.log", b"one\nNEEDLE two\n")]);
    let (out, _, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("logs/x.log:2:NEEDLE two"), "got: {out}");
    assert_eq!(c, Some(0));
}

/// The trap: a nesting check naming only gzip would hand this to the searcher
/// as binary and silently find nothing.
#[test]
fn a_zst_member_inside_a_tar_zst_is_decompressed() {
    let f = helpers::tar_zst_nested_zst("n.tar.zst", "logs/access.log.1.zst", b"NEEDLE nested\n");
    let (out, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("NEEDLE nested"), "nested .zst not decompressed: {out} / {err}");
    assert_eq!(c, Some(0));
}

/// A host mid-migration: gzip outer, zstd member.
#[test]
fn a_zst_member_inside_a_tgz_is_decompressed() {
    let f = helpers::tgz_nested_zst("m.tgz", "logs/access.log.1.zst", b"NEEDLE mixed\n");
    let (out, _, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("NEEDLE mixed"), "got: {out}");
    assert_eq!(c, Some(0));
}

#[test]
fn nesting_is_still_capped_at_one_level() {
    let inner = zstd::stream::encode_all(&b"NEEDLE deep\n"[..], 3).unwrap();
    let f = helpers::tar_zst_nested_zst("d2.tar.zst", "logs/a.log.zst", &inner);
    let (out, _, _) = run(&["NEEDLE deep", f.path.to_str().unwrap()]);
    assert!(!out.contains("NEEDLE deep"), "recursed past one level; bomb guard gone: {out}");
}

#[test]
fn a_truncated_tar_zst_is_two_not_one() {
    let big = vec![b'x'; 400_000];
    let f = helpers::tar_zst_truncated("t.tar.zst", &[("logs/a.log", &big)], 0.4);
    let (_, err, c) = run(&["NEEDLE-absent", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(2), "an unreadable .tar.zst must never look like a clean 1");
    assert!(!err.is_empty(), "must diagnose on stderr");
}

#[test]
fn all_inflate_modes_agree_on_a_tar_zst() {
    let f = helpers::tar_zst("i.tar.zst", &[("logs/a.log", b"NEEDLE one\n")]);
    let p = f.path.to_str().unwrap();
    let a = run(&["--inflate", "auto", "NEEDLE", p]).0;
    let s = run(&["--inflate", "stream", "NEEDLE", p]).0;
    let b = run(&["--inflate", "buffer", "NEEDLE", p]).0;
    assert_eq!(a, s, "auto and stream disagreed on zstd");
    assert_eq!(a, b, "buffer did not fall back cleanly on zstd");
    assert!(a.contains("NEEDLE one"));
}

#[test]
fn a_binary_member_in_a_tar_zst_is_still_skipped_without_dash_a() {
    let mut bin = b"NEEDLE".to_vec();
    bin.extend_from_slice(&[0u8, 1, 2, 0, 0]);
    bin.push(b'\n');
    let f = helpers::tar_zst("b.tar.zst", &[("logs/blob.bin", &bin)]);
    let (out, _, _) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(!out.contains("blob.bin:1:"), "binary member should be skipped: {out}");
    let (out_a, _, c) = run(&["-a", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(out_a.contains("blob.bin"), "-a must force it: {out_a}");
    assert_eq!(c, Some(0));
}
