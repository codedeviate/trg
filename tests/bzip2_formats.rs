mod helpers;

use std::process::Command;

fn run(args: &[&str]) -> (String, String, Option<i32>) {
    let o = Command::new(env!("CARGO_BIN_EXE_trg")).args(args).output().unwrap();
    (String::from_utf8_lossy(&o.stdout).into_owned(),
     String::from_utf8_lossy(&o.stderr).into_owned(),
     o.status.code())
}

/// The differential oracle, as for the other formats: extracted in-process,
/// rg on the plain files is the authority on per-member line numbers.
#[test]
fn agrees_with_rg_on_line_numbers_in_a_tar_bz2() {
    let members: &[(&str, &[u8])] = &[
        ("logs/a.log", b"one\ntwo NEEDLE\nthree\nfour NEEDLE\n"),
        ("logs/b.log", b"NEEDLE first line\nnope\n"),
    ];
    let f = helpers::tar_bz2("d.tar.bz2", members);
    let ex = tempfile::tempdir().unwrap();
    tar::Archive::new(&helpers::tar_bytes(members)[..]).unpack(ex.path()).unwrap();

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
    assert_eq!(got, want, "trg disagreed with rg on a .tar.bz2");
}

#[test]
fn a_tar_bz2_searches_like_a_tgz() {
    let f = helpers::tar_bz2("a.tar.bz2", &[("logs/x.log", b"one\nNEEDLE two\n")]);
    let (out, _, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("logs/x.log:2:NEEDLE two"), "got: {out}");
    assert_eq!(c, Some(0));
}

/// A plain `.bz2` log, not a tar: searched as one stream.
#[test]
fn a_bare_bz2_log_is_searched() {
    let f = helpers::raw("access.log.bz2", &helpers::bz2_bytes(b"one\nNEEDLE bare\n"));
    let (out, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("NEEDLE bare"), "got: {out} / {err}");
    assert_eq!(c, Some(0));
}

/// The trap the nesting check has caught for every format: naming only some
/// of them would hand this to the searcher as binary and find nothing.
#[test]
fn a_bz2_member_inside_a_tar_bz2_is_decompressed() {
    let inner = helpers::bz2_bytes(b"NEEDLE nested\n");
    let f = helpers::tar_bz2("n.tar.bz2", &[("logs/access.log.1.bz2", &inner)]);
    let (out, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("NEEDLE nested"), "nested .bz2 not decompressed: {out} / {err}");
    assert_eq!(c, Some(0));
}

/// logrotate with `compresscmd bzip2` under a tgz archiver.
#[test]
fn a_bz2_member_inside_a_tgz_is_decompressed() {
    let inner = helpers::bz2_bytes(b"NEEDLE mixed\n");
    let f = helpers::tgz("m.tgz", &[("logs/access.log.1.bz2", &inner)]);
    let (out, _, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("NEEDLE mixed"), "got: {out}");
    assert_eq!(c, Some(0));
}

#[test]
fn nesting_is_still_capped_at_one_level() {
    let inner = helpers::bz2_bytes(&helpers::bz2_bytes(b"NEEDLE deep\n"));
    let f = helpers::tar_bz2("d2.tar.bz2", &[("logs/a.log.bz2", &inner)]);
    let (out, _, _) = run(&["NEEDLE deep", f.path.to_str().unwrap()]);
    assert!(!out.contains("NEEDLE deep"), "recursed past one level; bomb guard gone: {out}");
}

#[test]
fn a_truncated_tar_bz2_is_two_not_one() {
    let big: Vec<u8> = (0..400_000u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
    let f = helpers::tar_bz2_truncated("t.tar.bz2", &[("logs/a.log", &big)], 0.4);
    let (_, err, c) = run(&["NEEDLE-absent", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(2), "an unreadable .tar.bz2 must never look like a clean 1");
    assert!(!err.is_empty(), "must diagnose on stderr");
}

/// pbzip2 and lbzip2 write one stream per chunk. A single-stream decoder
/// reads the first chunk, stops, and every member after it is never seen —
/// an exit 1 about the rest of the archive.
#[test]
fn a_parallel_bzip2_archive_is_read_to_the_end() {
    let pad = vec![b'x'; 300_000];
    let f = helpers::tar_bz2_multistream("p.tar.bz2", &[
        ("logs/a.log", b"NEEDLE in the first stream\n"),
        ("logs/pad.log", &pad),
        ("logs/z.log", b"NEEDLE in the last stream\n"),
    ], 100_000);
    let (out, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("first stream"), "got: {out} / {err}");
    assert!(out.contains("last stream"), "stopped after the first bzip2 stream: {out} / {err}");
    assert_eq!(c, Some(0));
}

#[test]
fn a_binary_member_in_a_tar_bz2_is_still_skipped_without_dash_a() {
    let mut bin = b"NEEDLE".to_vec();
    bin.extend_from_slice(&[0u8, 1, 2, 0, 0]);
    bin.push(b'\n');
    let f = helpers::tar_bz2("b.tar.bz2", &[("logs/blob.bin", &bin)]);
    let (out, _, _) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(!out.contains("blob.bin:1:"), "binary member should be skipped: {out}");
    let (out_a, _, c) = run(&["-a", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(out_a.contains("blob.bin"), "-a must force it: {out_a}");
    assert_eq!(c, Some(0));
}

/// What the real tool writes, when it is installed: the fixtures above come
/// from the same library trg reads with, which proves less.
#[test]
fn reads_what_the_bzip2_binary_writes() {
    if Command::new("bzip2").arg("--help").output().is_err() {
        return;
    }
    let tar = helpers::tar_bytes(&[("logs/x.log", b"one\nNEEDLE from bzip2(1)\n")]);
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("cli.tar");
    std::fs::write(&p, tar).unwrap();
    assert!(Command::new("bzip2").arg(&p).status().unwrap().success());
    let (out, err, c) = run(&["NEEDLE", d.path().join("cli.tar.bz2").to_str().unwrap()]);
    assert!(out.contains("logs/x.log:2:NEEDLE from bzip2(1)"), "got: {out} / {err}");
    assert_eq!(c, Some(0));
}
