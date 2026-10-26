//! Unix `compress` (`.Z`), through stuffr's decoder.

mod helpers;

use std::process::Command;

fn run(args: &[&str]) -> (String, String, Option<i32>) {
    let o = Command::new(env!("CARGO_BIN_EXE_trg")).args(args).output().unwrap();
    (String::from_utf8_lossy(&o.stdout).into_owned(),
     String::from_utf8_lossy(&o.stderr).into_owned(),
     o.status.code())
}

#[test]
fn a_tar_z_searches_like_a_tgz() {
    let tar = helpers::tar_bytes(&[("logs/x.log", b"one\nNEEDLE two\n"), ("logs/y.log", b"NEEDLE first\n")]);
    let f = helpers::raw("a.tar.Z", &helpers::z_bytes(&tar));
    let (out, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("logs/x.log:2:NEEDLE two"), "got: {out} / {err}");
    assert!(out.contains("logs/y.log:1:NEEDLE first"), "got: {out}");
    assert_eq!(c, Some(0));
}

/// Big enough that the 9-bit codes grow to 16 and the table resets: the
/// parts of the format a tiny fixture never reaches.
#[test]
fn a_large_z_file_is_read_to_the_end() {
    let mut body: Vec<u8> = (0..300_000u32).flat_map(|i| format!("{i} GET /item-{} 200\n", i * 7919 % 10007).into_bytes()).collect();
    body.extend_from_slice(b"NEEDLE at the very end\n");
    let f = helpers::raw("big.log.Z", &helpers::z_bytes(&body));
    let (out, err, c) = run(&["-n", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains(":300001:NEEDLE at the very end"), "got: {out} / {err}");
    assert_eq!(c, Some(0));
}

/// `.Z` records no length and no checksum, so a truncated one decodes
/// cleanly to a shorter file: cut at every seventh byte of a 54 KB `.Z`,
/// all 7,786 cuts exited 1. The 1 stands, since the file gives nothing to
/// contradict it, but it comes with a note, so whoever acts on "clean" knows
/// this file could not vouch for its own end.
#[test]
fn no_match_in_a_z_file_is_one_with_a_note_that_it_cannot_prove_completeness() {
    let f = helpers::raw("n.log.Z", &helpers::z_bytes(b"nothing here\n"));
    let (out, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert_eq!((out.as_str(), c), ("", Some(1)));
    assert!(err.contains("n.log.Z") && err.contains("truncat"), "got: {err}");
}

/// A match is a match however much of the file is there, so no note.
#[test]
fn a_match_in_a_z_file_needs_no_note() {
    let f = helpers::raw("m.log.Z", &helpers::z_bytes(b"NEEDLE\n"));
    let (_, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert_eq!((err.as_str(), c), ("", Some(0)));
}

#[test]
fn a_truncated_z_file_is_noted_not_passed_silently() {
    let body: Vec<u8> = (0..20_000u32).flat_map(|i| format!("line {i} GET /x\n").into_bytes()).collect();
    let full = helpers::z_bytes(&body);
    let f = helpers::raw("t.log.Z", &full[..full.len() / 2]);
    let (_, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(1), "the format gives no way to tell, so this stays a 1");
    assert!(err.contains("truncat"), "but never silently: {err}");
}

/// A `.Z` header asking for codes wider than 16 bits is not a `.Z` any tool
/// wrote. Unreadable is a 2.
#[test]
fn a_corrupt_z_header_is_two() {
    let f = helpers::raw("bad.Z", &[0x1f, 0x9d, 0x9f, 0x41, 0x42, 0x43]);
    let (_, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(2), "got: {err}");
    assert!(!err.is_empty());
}

/// stuffr's `.Z` decoder only takes a reader it can own, so a `.Z` member
/// inside an archive is not decompressed. It must say so and exit 2, not be
/// searched as binary and come back a clean 1.
#[test]
fn a_z_member_inside_an_archive_is_two_not_one() {
    let inner = helpers::z_bytes(b"NEEDLE in a nested .Z\n");
    let f = helpers::tgz("m.tgz", &[("logs/access.log.1.Z", &inner), ("logs/plain.log", b"NEEDLE plain\n")]);
    let (out, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("NEEDLE plain"), "the rest of the archive is still searched: {out}");
    assert_eq!(c, Some(2), "got: {out} / {err}");
    assert!(err.contains("access.log.1.Z") && err.contains(".Z"), "got: {err}");

    let z = helpers::zip("m.zip", &[("access.log.1.Z", &inner)]);
    let (_, err, c) = run(&["NEEDLE", z.path.to_str().unwrap()]);
    assert_eq!(c, Some(2), "inside a zip too: {err}");
}

#[test]
fn reads_what_the_compress_binary_writes() {
    if Command::new("compress").arg("-V").output().is_err() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("cli.log");
    let body: String = (0..20_000).map(|i| format!("line {i}\n")).collect::<String>() + "NEEDLE from compress(1)\n";
    std::fs::write(&p, body).unwrap();
    assert!(Command::new("compress").arg("-f").arg(&p).status().unwrap().success());
    let (out, err, c) = run(&["NEEDLE", d.path().join("cli.log.Z").to_str().unwrap()]);
    assert!(out.contains("NEEDLE from compress(1)"), "got: {out} / {err}");
    assert_eq!(c, Some(0));
}
