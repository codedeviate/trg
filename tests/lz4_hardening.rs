//! lz4 framing the decoder got wrong: legacy frames one after another, the
//! legacy format's missing end mark, and a skippable frame longer than the
//! sniff window.

mod helpers;

use helpers::{lz4_bytes, lz4_legacy_bytes, run_trg, run_trg_stdin};

/// A skippable frame of `len` zero bytes, then `bytes`.
fn behind_long_skippable(len: u32, bytes: &[u8]) -> Vec<u8> {
    let mut out = vec![0x50, 0x2A, 0x4D, 0x18];
    out.extend_from_slice(&len.to_le_bytes());
    out.resize(out.len() + len as usize, 0);
    out.extend_from_slice(bytes);
    out
}

/// `lz4 -l a; lz4 -l b; cat a.lz4 b.lz4`: `lz4 -dc` reads both. The second
/// magic stood where a block size is read, and was a block too big.
#[test]
fn concatenated_legacy_frames_are_all_searched() {
    let mut two = lz4_legacy_bytes(b"NEEDLE first frame\n");
    two.extend(lz4_legacy_bytes(b"NEEDLE second frame\n"));
    two.extend(lz4_legacy_bytes(b"NEEDLE third frame\n"));
    let f = helpers::raw("two.lz4", &two);
    let (out, err, c) = run_trg(&["NEEDLE", f.path.to_str().unwrap()]);
    assert_eq!(out.lines().count(), 3, "got: {out} / {err}");
    assert!(out.contains("NEEDLE third frame"), "got: {out} / {err}");
    assert_eq!(c, Some(0), "got: {err}");
    assert!(err.is_empty(), "got: {err}");
}

#[test]
fn a_legacy_frame_and_a_modern_one_in_either_order_are_both_searched() {
    let legacy = lz4_legacy_bytes(b"NEEDLE legacy\n");
    let modern = lz4_bytes(b"NEEDLE modern\n");
    for (label, bytes) in [
        ("legacy first", [legacy.clone(), modern.clone()].concat()),
        ("modern first", [modern.clone(), legacy.clone()].concat()),
        ("legacy between", [modern.clone(), legacy.clone(), modern.clone()].concat()),
    ] {
        let f = helpers::raw("mix.lz4", &bytes);
        let (out, err, c) = run_trg(&["NEEDLE", f.path.to_str().unwrap()]);
        assert!(out.contains("NEEDLE legacy") && out.contains("NEEDLE modern"), "{label}: {out} / {err}");
        assert_eq!(c, Some(0), "{label}: {err}");
    }
}

#[test]
fn concatenated_legacy_frames_in_a_member_are_all_searched() {
    let mut two = lz4_legacy_bytes(b"NEEDLE first frame\n");
    two.extend(lz4_legacy_bytes(b"NEEDLE second frame\n"));
    let f = helpers::tgz("c.tgz", &[("logs/a.log.lz4", &two)]);
    let (out, err, c) = run_trg(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains("first frame") && out.contains("second frame"), "got: {out} / {err}");
    assert_eq!(c, Some(0));
}

/// A frame cut inside its second legacy frame is still a cut.
#[test]
fn a_cut_inside_a_later_legacy_frame_is_two() {
    let mut two = lz4_legacy_bytes(b"first frame\n");
    two.extend(lz4_legacy_bytes(b"second frame, long enough to cut somewhere in the middle\n"));
    for drop in [1, 10, 30] {
        let f = helpers::raw("cut.lz4", &two[..two.len() - drop]);
        let (_, err, c) = run_trg(&["NEEDLE", f.path.to_str().unwrap()]);
        assert_eq!(c, Some(2), "{drop} bytes short: {err}");
    }
}

/// The legacy format has no end mark and no checksum: cut at a block
/// boundary, it is a shorter file. As for `.Z`, a 1 says so.
#[test]
fn no_match_in_a_legacy_lz4_notes_that_truncation_is_undetectable() {
    let f = helpers::raw("l.lz4", &lz4_legacy_bytes(b"nothing here\n"));
    let (out, err, c) = run_trg(&["NEEDLE", f.path.to_str().unwrap()]);
    assert_eq!((out.as_str(), c), ("", Some(1)), "got: {err}");
    assert!(err.contains("cannot detect truncation"), "got: {err}");
    assert!(err.contains("lz4"), "got: {err}");

    // A match is a 0 whatever was cut, and says nothing.
    let (_, err, c) = run_trg(&["nothing", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(0));
    assert!(err.is_empty(), "got: {err}");
}

/// Only where the stream ends in a legacy frame: a frame-format one after
/// it has its end mark.
#[test]
fn the_note_is_for_a_stream_that_ends_in_a_legacy_frame() {
    let legacy = lz4_legacy_bytes(b"nothing here\n");
    let modern = lz4_bytes(b"nor here\n");
    for (label, bytes, noted) in [
        ("modern", modern.clone(), false),
        ("legacy then modern", [legacy.clone(), modern.clone()].concat(), false),
        ("modern then legacy", [modern.clone(), legacy.clone()].concat(), true),
    ] {
        let f = helpers::raw("n.lz4", &bytes);
        let (_, err, c) = run_trg(&["NEEDLE", f.path.to_str().unwrap()]);
        assert_eq!(c, Some(1), "{label}: {err}");
        assert_eq!(err.contains("cannot detect truncation"), noted, "{label}: {err}");
    }
}

/// A skippable frame longer than the 512 bytes the format is read from was
/// taken for zstd whatever followed it.
#[test]
fn an_lz4_frame_behind_a_long_skippable_frame_is_read() {
    for (label, inner) in [("frame", lz4_bytes(b"x\nNEEDLE behind\n")), ("legacy", lz4_legacy_bytes(b"x\nNEEDLE behind\n"))] {
        let bytes = behind_long_skippable(1000, &inner);
        let f = helpers::raw("skip.lz4", &bytes);
        let (out, err, c) = run_trg(&["NEEDLE", f.path.to_str().unwrap()]);
        assert!(out.ends_with(":2:NEEDLE behind\n"), "{label}: {out} / {err}");
        assert_eq!(c, Some(0), "{label}");

        let (out, err, c) = run_trg_stdin(&["NEEDLE", "-"], &bytes);
        assert!(out.contains("NEEDLE behind"), "{label} from a pipe: {out} / {err}");
        assert_eq!(c, Some(0), "{label} from a pipe");

        let f = helpers::tgz("m.tgz", &[("logs/a.log.lz4", &bytes)]);
        let (out, err, c) = run_trg(&["NEEDLE", f.path.to_str().unwrap()]);
        assert!(out.contains("logs/a.log.lz4:2:NEEDLE behind"), "{label} as a member: {out} / {err}");
        assert_eq!(c, Some(0), "{label} as a member");
    }
}

/// Two skippable frames, the first past the window, the second short.
#[test]
fn several_skippable_frames_before_lz4_are_stepped_over() {
    let inner = behind_long_skippable(4, &lz4_bytes(b"NEEDLE behind two\n"));
    let f = helpers::raw("skip2.lz4", &behind_long_skippable(600, &inner));
    let (out, err, c) = run_trg(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.ends_with(":1:NEEDLE behind two\n"), "{out} / {err}");
    assert_eq!(c, Some(0));
}

#[test]
fn zstd_behind_a_long_skippable_frame_is_still_read() {
    let bytes = behind_long_skippable(1000, &helpers::zstd_plain_bytes(b"x\nNEEDLE zstd\n"));
    let f = helpers::raw("skip.zst", &bytes);
    let (out, err, c) = run_trg(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.ends_with(":2:NEEDLE zstd\n"), "{out} / {err}");
    assert_eq!(c, Some(0));
    let (_, err, c) = run_trg(&["ABSENT", f.path.to_str().unwrap()]);
    assert_eq!((err.as_str(), c), ("", Some(1)));
}

/// Nothing but a long skippable frame: read whole, holding nothing.
#[test]
fn a_long_skippable_frame_alone_is_one_and_a_cut_one_is_two() {
    let whole = behind_long_skippable(1000, b"");
    let f = helpers::raw("only.zst", &whole);
    let (_, err, c) = run_trg(&["NEEDLE", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(1), "got: {err}");
    let f = helpers::raw("cut.zst", &whole[..700]);
    let (_, err, c) = run_trg(&["NEEDLE", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(2), "got: {err}");
    assert!(!err.is_empty());
}
