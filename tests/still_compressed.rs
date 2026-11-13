//! One level of compression is removed from a file or a member, and no
//! more. What is under it, if still compressed, was searched as the binary
//! data it is: no match, exit 1, for lines never read. That is a 2.

mod helpers;

use helpers::run_trg;

const STILL: &str = "after one level was removed: not searched";

fn kinds() -> Vec<(&'static str, Vec<u8>)> {
    let body = b"one\nNEEDLE two\n";
    vec![
        ("gzip", helpers::gzip_bytes(body)),
        ("bzip2", helpers::bz2_bytes(body)),
        ("xz", helpers::xz_bytes(body)),
        ("zstd", helpers::zstd_plain_bytes(body)),
        ("lz4", helpers::lz4_bytes(body)),
        (".Z", helpers::z_bytes(body)),
    ]
}

/// `gzip x.log.gz` by a second logrotate rule: `x.log.gz.gz`.
#[test]
fn a_file_compressed_twice_is_two_and_says_so() {
    for (kind, inner) in kinds() {
        let f = helpers::raw("x.log.gz.gz", &helpers::gzip_bytes(&inner));
        let p = f.path.to_str().unwrap();
        let (out, err, c) = run_trg(&["NEEDLE", p]);
        assert_eq!(c, Some(2), "{kind}: {out} / {err}");
        assert!(err.contains(&format!("{p}: still compressed ({kind}) {STILL}")), "{kind}: {err}");
    }
}

#[test]
fn any_outer_format_over_a_compressed_file_is_two() {
    let inner = helpers::gzip_bytes(b"NEEDLE\n");
    for (kind, outer) in [
        ("bzip2", helpers::bz2_bytes(&inner)),
        ("xz", helpers::xz_bytes(&inner)),
        ("zstd", helpers::zstd_plain_bytes(&inner)),
        ("lz4", helpers::lz4_bytes(&inner)),
        (".Z", helpers::z_bytes(&inner)),
    ] {
        let f = helpers::raw("x.log.gz.x", &outer);
        let (_, err, c) = run_trg(&["NEEDLE", f.path.to_str().unwrap()]);
        assert_eq!(c, Some(2), "under {kind}: {err}");
        assert!(err.contains("still compressed (gzip)"), "under {kind}: {err}");
    }
}

/// The member is named, the other members are still searched, and the run
/// is a 2 whether or not they matched.
#[test]
fn a_member_compressed_twice_is_two_and_the_others_are_searched() {
    let twice = helpers::gzip_bytes(&helpers::gzip_bytes(b"NEEDLE hidden\n"));
    for (label, f) in [
        ("tgz", helpers::tgz("a.tgz", &[("logs/a.log.gz.gz", &twice), ("logs/b.log", b"NEEDLE seen\n")])),
        ("tar", helpers::tar_plain("a.tar", &[("logs/a.log.gz.gz", &twice), ("logs/b.log", b"NEEDLE seen\n")])),
        ("zip", helpers::zip("a.zip", &[("logs/a.log.gz.gz", &twice), ("logs/b.log", b"NEEDLE seen\n")])),
    ] {
        let p = f.path.to_str().unwrap();
        let (out, err, c) = run_trg(&["NEEDLE", p]);
        assert!(out.contains("logs/b.log:1:NEEDLE seen"), "{label}: {out} / {err}");
        assert!(!out.contains("hidden"), "{label}: {out}");
        assert!(
            err.contains(&format!("{p}:logs/a.log.gz.gz: still compressed (gzip) {STILL}")),
            "{label}: {err}"
        );
        assert_eq!(c, Some(2), "{label}");

        let (_, err, c) = run_trg(&["ABSENT", p]);
        assert_eq!(c, Some(2), "{label}, no match: {err}");
    }
}

/// One level is what trg removes, and one level is a clean read.
#[test]
fn one_level_is_still_silent() {
    for (kind, bytes) in kinds() {
        let f = helpers::raw("x.log.c", &bytes);
        let (_, err, c) = run_trg(&["ABSENT", f.path.to_str().unwrap()]);
        assert_eq!(c, Some(1), "{kind}: {err}");
        if kind != ".Z" {
            assert!(err.is_empty(), "{kind}: {err}");
        }
        let f = helpers::tgz("a.tgz", &[("logs/a.log.c", &bytes)]);
        let (_, err, c) = run_trg(&["ABSENT", f.path.to_str().unwrap()]);
        assert_eq!((err.as_str(), c), ("", Some(1)), "{kind} in a tgz");
    }
}

/// A member excluded by `-g` is not read, so nothing is said about it.
#[test]
fn a_member_the_globs_exclude_is_not_reported() {
    let twice = helpers::gzip_bytes(&helpers::gzip_bytes(b"NEEDLE hidden\n"));
    let f = helpers::tgz("a.tgz", &[("logs/a.log.gz.gz", &twice), ("logs/b.log", b"nothing\n")]);
    let (_, err, c) = run_trg(&["-g", "b.log", "NEEDLE", f.path.to_str().unwrap()]);
    assert_eq!((err.as_str(), c), ("", Some(1)));
}
