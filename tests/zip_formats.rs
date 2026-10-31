//! `.zip` through stuffr's zip container. A zip's members are handed out one
//! at a time like a tar's, so everything per-member — line numbers, `-g`,
//! binary detection, one level of nested decompression — carries over.

mod helpers;

use std::process::Command;

use helpers::ZipMethod;

fn run(args: &[&str]) -> (String, String, Option<i32>) {
    let o = Command::new(env!("CARGO_BIN_EXE_trg")).args(args).output().unwrap();
    (String::from_utf8_lossy(&o.stdout).into_owned(),
     String::from_utf8_lossy(&o.stderr).into_owned(),
     o.status.code())
}

#[test]
fn a_zip_searches_like_a_tgz() {
    let f = helpers::zip("a.zip", &[
        ("logs/x.log", b"one\nNEEDLE two\n"),
        ("logs/y.log", b"NEEDLE first\nnope\n"),
    ]);
    let p = f.path.to_str().unwrap();
    let (out, err, c) = run(&["NEEDLE", p]);
    assert!(out.contains(&format!("{p}:logs/x.log:2:NEEDLE two")), "got: {out} / {err}");
    assert!(out.contains(&format!("{p}:logs/y.log:1:NEEDLE first")), "got: {out}");
    assert_eq!(c, Some(0));
    assert!(err.is_empty(), "got: {err}");
}

#[test]
fn every_common_zip_method_is_read() {
    for (method, label) in [(ZipMethod::Stored, "stored"), (ZipMethod::Deflated, "deflated"), (ZipMethod::Bzip2, "bzip2")] {
        let bytes = helpers::zip_bytes(&[("a.log", b"x\nNEEDLE here\n")], method);
        let f = helpers::raw("m.zip", &bytes);
        let (out, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
        assert!(out.contains("a.log:2:NEEDLE here"), "{label}: {out} / {err}");
        assert_eq!(c, Some(0), "{label}");
    }
}

#[test]
fn no_match_in_a_whole_zip_is_one() {
    let f = helpers::zip("n.zip", &[("logs/a.log", b"nothing\n")]);
    let (out, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert_eq!((out.as_str(), err.as_str(), c), ("", "", Some(1)));
}

/// `zip` with nothing in it is 22 bytes of end-of-central-directory. It was
/// read completely and holds nothing, which is a 1, not an error.
#[test]
fn an_empty_zip_is_one() {
    let f = helpers::raw("e.zip", &helpers::zip_bytes(&[], ZipMethod::Deflated));
    let (_, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(1), "got: {err}");
    assert!(err.is_empty(), "got: {err}");
}

#[test]
fn member_globs_apply_inside_a_zip() {
    let f = helpers::zip("g.zip", &[
        ("logs/access.log", b"NEEDLE access\n"),
        ("logs/error.log", b"NEEDLE error\n"),
    ]);
    let (out, _, c) = run(&["-g", "access.log", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("NEEDLE access") && !out.contains("NEEDLE error"), "got: {out}");
    assert_eq!(c, Some(0));
}

/// Directory entries are structure, not members: `-T` lists files only, as
/// it does for a tar.
#[test]
fn dash_t_lists_zip_files_and_not_directories() {
    let f = helpers::zip("t.zip", &[("logs/a.log", b"x\n"), ("logs/deep/b.log", b"y\n")]);
    let p = f.path.to_str().unwrap();
    let (out, err, c) = run(&["-T", p]);
    assert_eq!(out, format!("{p}:logs/a.log\n{p}:logs/deep/b.log\n"), "stderr: {err}");
    assert_eq!(c, Some(0));
}

/// A logrotate'd `.gz` zipped up for a support ticket.
#[test]
fn a_compressed_member_inside_a_zip_is_decompressed() {
    let gz = {
        use std::io::Write;
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(b"NEEDLE in a gz in a zip\n").unwrap();
        e.finish().unwrap()
    };
    let zst = zstd::stream::encode_all(&b"NEEDLE in a zst in a zip\n"[..], 3).unwrap();
    let f = helpers::zip("z.zip", &[("access.log.1.gz", &gz), ("access.log.2.zst", &zst)]);
    let (out, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("access.log.1.gz:1:NEEDLE in a gz in a zip"), "got: {out} / {err}");
    assert!(out.contains("access.log.2.zst:1:NEEDLE in a zst in a zip"), "got: {out} / {err}");
    assert_eq!(c, Some(0));
}

#[test]
fn a_binary_member_in_a_zip_is_still_skipped_without_dash_a() {
    let mut bin = b"NEEDLE".to_vec();
    bin.extend_from_slice(&[0u8, 1, 2, 0, 0, b'\n']);
    let f = helpers::zip("b.zip", &[("blob.bin", &bin)]);
    let (out, _, _) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(!out.contains("blob.bin:1:"), "binary member should be skipped: {out}");
    let (out_a, _, c) = run(&["-a", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(out_a.contains("blob.bin"), "-a must force it: {out_a}");
    assert_eq!(c, Some(0));
}

/// A zip cut off loses its central directory, which is at the end. Nothing
/// can be trusted to be complete, so it is a 2 — never a clean 1.
#[test]
fn a_truncated_zip_is_two_not_one() {
    let bytes = helpers::zip_bytes(&[("a.log", &vec![b'x'; 50_000][..]), ("b.log", b"tail\n")], ZipMethod::Deflated);
    let f = helpers::raw("t.zip", &bytes[..bytes.len() * 2 / 3]);
    let (_, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(2), "a truncated zip must never look like a clean 1: {err}");
    assert!(!err.is_empty(), "must diagnose on stderr");
}

/// A member whose data was damaged after it was written: its CRC no longer
/// matches. The bytes before the damage may already have been searched, but
/// the member was not read correctly, so the run is a 2.
#[test]
fn a_corrupt_zip_member_is_two() {
    let body: Vec<u8> = (0..20_000u32).flat_map(|i| format!("line {i}\n").into_bytes()).collect();
    let mut bytes = helpers::zip_bytes(&[("a.log", &body)], ZipMethod::Stored);
    // Stored, so the payload is the text itself: flip a byte in the middle of
    // it, well clear of the headers.
    let at = bytes.windows(10).position(|w| w == b"line 10000").unwrap();
    bytes[at] ^= 0x20;
    let f = helpers::raw("c.zip", &bytes);
    let (_, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(2), "a CRC mismatch must not be a clean 1: {err}");
    assert!(!err.is_empty());
}

/// Encrypted members cannot be read, so they cannot be called clean — and
/// one of them is not the end of the archive: the members after it are still
/// searched.
#[test]
fn an_encrypted_member_is_two_and_the_rest_is_still_searched() {
    if Command::new("zip").arg("-v").output().is_err() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("secret.log"), "NEEDLE behind a password\n").unwrap();
    std::fs::write(d.path().join("plain.log"), "NEEDLE in the open\n").unwrap();
    let zip = |args: &[&str]| {
        assert!(Command::new("zip").args(args).current_dir(d.path()).status().unwrap().success());
    };
    zip(&["-q", "-P", "pw", "s.zip", "secret.log"]);
    zip(&["-q", "s.zip", "plain.log"]);
    let (out, err, c) = run(&["NEEDLE", d.path().join("s.zip").to_str().unwrap()]);
    assert_eq!(c, Some(2), "got: {out} / {err}");
    assert!(err.contains("zip entry 1") && err.to_lowercase().contains("password"), "got: {err}");
    assert!(out.contains("plain.log:1:NEEDLE in the open"), "stopped at the encrypted member: {out}");
    assert!(!out.contains("behind a password"));
}

/// What Info-ZIP writes, when it is installed — `zip -r`, the usual way a
/// directory of logs ends up in a zip.
#[test]
fn reads_what_the_zip_binary_writes() {
    if Command::new("zip").arg("-v").output().is_err() {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("logs")).unwrap();
    std::fs::write(d.path().join("logs/a.log"), "one\nNEEDLE from zip(1)\n").unwrap();
    let ok = Command::new("zip").args(["-q", "-r", "l.zip", "logs"])
        .current_dir(d.path()).status().unwrap().success();
    assert!(ok);
    let (out, err, c) = run(&["NEEDLE", d.path().join("l.zip").to_str().unwrap()]);
    assert!(out.contains("logs/a.log:2:NEEDLE from zip(1)"), "got: {out} / {err}");
    assert_eq!(c, Some(0));
}

/// Archives found by walking a directory are picked by content, so a zip in
/// a directory of tgzs is searched with the rest.
#[test]
fn a_zip_in_a_walked_directory_is_searched() {
    let d = tempfile::tempdir().unwrap();
    let z = helpers::zip("x.zip", &[("a.log", b"NEEDLE zipped\n")]);
    std::fs::copy(&z.path, d.path().join("x.zip")).unwrap();
    let t = helpers::tgz("y.tgz", &[("a.log", b"NEEDLE tarred\n")]);
    std::fs::copy(&t.path, d.path().join("y.tgz")).unwrap();
    let (out, err, c) = run(&["NEEDLE", d.path().to_str().unwrap()]);
    assert!(out.contains("NEEDLE zipped") && out.contains("NEEDLE tarred"), "got: {out} / {err}");
    assert_eq!(c, Some(0));
}

/// A "cloaked" zip, whose local header names a different file from its
/// central directory, shows one name to `unzip` and another to a streaming
/// reader (GHSA-m665-v7pm-7grj). trg reads by the central directory, as
/// unzip does, and says the headers disagree: a 2, since which member was
/// meant is not something trg can know.
#[test]
fn a_zip_whose_headers_disagree_is_two() {
    let mut bytes = helpers::zip_bytes(&[("good.log", b"NEEDLE cloaked\n")], ZipMethod::Stored);
    // The local header's name follows its 30 fixed bytes.
    assert_eq!(&bytes[30..38], b"good.log");
    bytes[30..38].copy_from_slice(b"evil.log");
    let z = helpers::raw("cloak.zip", &bytes);
    let (out, err, c) = run(&["NEEDLE", z.path.to_str().unwrap()]);
    assert_eq!(c, Some(2), "got: {out} / {err}");
    assert!(out.contains("good.log:1:NEEDLE cloaked"), "the central directory's name: {out}");
    assert!(err.contains("disagree on name for entry `good.log`"), "got: {err}");
}
