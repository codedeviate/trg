mod helpers;

use std::process::Command;

fn run(args: &[&str]) -> (String, String, Option<i32>) {
    let o = Command::new(env!("CARGO_BIN_EXE_trg")).args(args).output().unwrap();
    (String::from_utf8_lossy(&o.stdout).into_owned(),
     String::from_utf8_lossy(&o.stderr).into_owned(),
     o.status.code())
}

#[test]
fn a_tar_lz4_searches_like_a_tgz() {
    let f = helpers::tar_lz4("a.tar.lz4", &[
        ("logs/x.log", b"one\nNEEDLE two\n"),
        ("logs/y.log", b"NEEDLE first\n"),
    ]);
    let (out, _, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("logs/x.log:2:NEEDLE two"), "got: {out}");
    assert!(out.contains("logs/y.log:1:NEEDLE first"), "got: {out}");
    assert_eq!(c, Some(0));
}

#[test]
fn an_lz4_member_inside_a_tgz_is_decompressed() {
    let inner = helpers::lz4_bytes(b"NEEDLE mixed\n");
    let f = helpers::tgz("m.tgz", &[("logs/access.log.1.lz4", &inner)]);
    let (out, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("NEEDLE mixed"), "nested .lz4 not decompressed: {out} / {err}");
    assert_eq!(c, Some(0));
}

#[test]
fn an_lz4_member_inside_a_tar_lz4_is_decompressed() {
    let inner = helpers::lz4_bytes(b"NEEDLE nested\n");
    let f = helpers::tar_lz4("n.tar.lz4", &[("logs/access.log.1.lz4", &inner)]);
    let (out, _, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("NEEDLE nested"), "got: {out}");
    assert_eq!(c, Some(0));
}

#[test]
fn nesting_is_still_capped_at_one_level() {
    let inner = helpers::lz4_bytes(&helpers::lz4_bytes(b"NEEDLE deep\n"));
    let f = helpers::tar_lz4("d2.tar.lz4", &[("logs/a.log.lz4", &inner)]);
    let (out, _, _) = run(&["NEEDLE deep", f.path.to_str().unwrap()]);
    assert!(!out.contains("NEEDLE deep"), "recursed past one level; bomb guard gone: {out}");
}

#[test]
fn a_truncated_tar_lz4_is_two_not_one() {
    let big: Vec<u8> = (0..400_000u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
    let f = helpers::tar_lz4_truncated("t.tar.lz4", &[("logs/a.log", &big)], 0.4);
    let (_, err, c) = run(&["NEEDLE-absent", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(2), "an unreadable .tar.lz4 must never look like a clean 1");
    assert!(!err.is_empty(), "must diagnose on stderr");
}

/// `cat a.lz4 b.lz4` is valid lz4 and `lz4 -d` decodes all of it.
#[test]
fn concatenated_lz4_frames_are_all_searched() {
    let mut two = helpers::lz4_bytes(b"NEEDLE first frame\n");
    two.extend_from_slice(&helpers::lz4_bytes(b"NEEDLE second frame\n"));
    let f = helpers::tgz("c.tgz", &[("logs/access.log.1.lz4", &two)]);
    let (out, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("NEEDLE first frame"), "got: {out} / {err}");
    assert!(out.contains("NEEDLE second frame"), "second frame dropped: {out} / {err}");
    assert_eq!(c, Some(0));
}

/// `lz4 -l`, the kernel's format. Large enough for two legacy blocks, so the
/// second one is proof the decoder kept going.
#[test]
fn the_legacy_lz4_format_is_read() {
    let mut tar_body = vec![b'x'; 9 << 20];
    tar_body.extend_from_slice(b"\nNEEDLE past the first block\n");
    let tar = helpers::tar_bytes(&[("logs/a.log", b"NEEDLE legacy\n"), ("logs/big.log", &tar_body)]);
    let f = helpers::raw("l.tar.lz4", &helpers::lz4_legacy_bytes(&tar));
    let (out, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("logs/a.log:1:NEEDLE legacy"), "got: {out} / {err}");
    assert!(out.contains("NEEDLE past the first block"), "stopped after one legacy block: {err}");
    assert_eq!(c, Some(0));
}

/// lz4 shares zstd's skippable-frame range. Read as zstd, this was a decode
/// error; read as text, an exit 1 about compressed bytes.
#[test]
fn a_leading_skippable_frame_before_lz4_is_stepped_over() {
    let lz = helpers::lz4_bytes(&helpers::tar_bytes(&[("logs/a.log", b"NEEDLE skipped to\n")]));
    let f = helpers::raw("s.tar.lz4", &helpers::with_skippable_frame(&lz));
    let (out, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("NEEDLE skipped to"), "got: {out} / {err}");
    assert_eq!(c, Some(0));

    // And one level down.
    let inner = helpers::with_skippable_frame(&helpers::lz4_bytes(b"NEEDLE inner skip\n"));
    let f = helpers::tgz("s.tgz", &[("logs/a.log.lz4", &inner)]);
    let (out, err, _) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("NEEDLE inner skip"), "got: {out} / {err}");
}

#[test]
fn reads_what_the_lz4_binary_writes() {
    if Command::new("lz4").arg("--version").output().is_err() {
        return;
    }
    let tar = helpers::tar_bytes(&[("logs/x.log", b"one\nNEEDLE from lz4(1)\n")]);
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("cli.tar");
    std::fs::write(&p, tar).unwrap();
    for (flag, out_name) in [("-z", "cli.tar.lz4"), ("-l", "cli.legacy.tar.lz4")] {
        let dest = d.path().join(out_name);
        assert!(Command::new("lz4").args(["-q", "-f", flag]).arg(&p).arg(&dest).status().unwrap().success());
        let (out, err, c) = run(&["NEEDLE", dest.to_str().unwrap()]);
        assert!(out.contains("logs/x.log:2:NEEDLE from lz4(1)"), "lz4 {flag}: {out} / {err}");
        assert_eq!(c, Some(0), "lz4 {flag}");
    }
}
