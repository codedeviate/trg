//! A zip is read from its directory, and these are the files whose
//! directory does not cover them: bytes in front of the zip, between its
//! entries, after its end record; an end record that counts fewer entries
//! than the directory lists; entries with data under a kind that has none.
//! Each was an exit 1 about bytes never read, where `unzip` warns or prints
//! the data.

mod helpers;

use std::process::Command;

use helpers::{run_trg, ZipMethod};

const HIDDEN: &[u8] = b"secret NEEDLE line\n";

/// One entry of a hand-built zip, stored.
#[derive(Clone)]
struct Entry {
    name: &'static [u8],
    data: &'static [u8],
    /// What the directory says the data is, where it differs.
    listed: Option<&'static [u8]>,
    /// External attributes and "version made by", as a Unix writer sets
    /// them for a symlink.
    attrs: u32,
    made_by: u16,
    /// Bit 3: sizes in a data descriptor after the data.
    described: bool,
}

fn entry(name: &'static [u8], data: &'static [u8]) -> Entry {
    Entry { name, data, listed: None, attrs: 0, made_by: 20, described: false }
}

fn crc(data: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(data);
    c.sum()
}

fn le16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn le32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn local(e: &Entry) -> Vec<u8> {
    let mut b = b"PK\x03\x04".to_vec();
    le16(&mut b, 20);
    le16(&mut b, if e.described { 8 } else { 0 });
    for _ in 0..3 {
        le16(&mut b, 0); // method, time, date
    }
    if e.described {
        b.extend_from_slice(&[0; 12]);
    } else {
        le32(&mut b, crc(e.data));
        le32(&mut b, e.data.len() as u32);
        le32(&mut b, e.data.len() as u32);
    }
    le16(&mut b, e.name.len() as u16);
    le16(&mut b, 0);
    b.extend_from_slice(e.name);
    b.extend_from_slice(e.data);
    if e.described {
        b.extend_from_slice(b"PK\x07\x08");
        le32(&mut b, crc(e.data));
        le32(&mut b, e.data.len() as u32);
        le32(&mut b, e.data.len() as u32);
    }
    b
}

fn central(e: &Entry, offset: u32) -> Vec<u8> {
    let data = e.listed.unwrap_or(e.data);
    let mut b = b"PK\x01\x02".to_vec();
    le16(&mut b, e.made_by);
    le16(&mut b, 20);
    le16(&mut b, if e.described { 8 } else { 0 });
    for _ in 0..3 {
        le16(&mut b, 0);
    }
    le32(&mut b, crc(data));
    le32(&mut b, data.len() as u32);
    le32(&mut b, data.len() as u32);
    le16(&mut b, e.name.len() as u16);
    for _ in 0..4 {
        le16(&mut b, 0); // extra, comment, disk, internal attributes
    }
    le32(&mut b, e.attrs);
    le32(&mut b, offset);
    b.extend_from_slice(e.name);
    b
}

fn end_record(count: u16, dir_size: u32, dir_offset: u32, comment: &[u8]) -> Vec<u8> {
    let mut b = b"PK\x05\x06".to_vec();
    le16(&mut b, 0);
    le16(&mut b, 0);
    le16(&mut b, count);
    le16(&mut b, count);
    le32(&mut b, dir_size);
    le32(&mut b, dir_offset);
    le16(&mut b, comment.len() as u16);
    b.extend_from_slice(comment);
    b
}

/// A zip of `locals`, whose directory lists `listed` (indices into
/// `locals`), with `gap` between the last entry and the directory, and an
/// end record counting `count` entries (the directory's own number if
/// `None`). Offsets are shifted by `shift`, as for a zip written behind
/// that many bytes.
fn build(locals: &[Entry], listed: &[usize], gap: &[u8], count: Option<u16>, shift: u32) -> Vec<u8> {
    let mut body = Vec::new();
    let mut offsets = Vec::new();
    for e in locals {
        offsets.push(body.len() as u32 + shift);
        body.extend(local(e));
    }
    body.extend_from_slice(gap);
    let dir: Vec<u8> = listed.iter().flat_map(|&i| central(&locals[i], offsets[i])).collect();
    let dir_offset = body.len() as u32 + shift;
    body.extend_from_slice(&dir);
    body.extend(end_record(count.unwrap_or(listed.len() as u16), dir.len() as u32, dir_offset, b""));
    body
}

fn plain() -> Vec<u8> {
    build(&[entry(b"a.log", b"hello\n")], &[0], b"", None, 0)
}

/// `trg NEEDLE` on `bytes` as a file: stdout, stderr, exit code.
fn search(name: &str, bytes: &[u8]) -> (String, String, Option<i32>) {
    let f = helpers::raw(name, bytes);
    run_trg(&["NEEDLE", f.path.to_str().unwrap()])
}

/// The builder's own zips are sound, or nothing below means anything.
#[test]
fn the_hand_built_zip_is_a_clean_read() {
    let (out, err, c) = search("plain.zip", &plain());
    assert_eq!((out.as_str(), err.as_str(), c), ("", "", Some(1)));
    let two = build(&[entry(b"a.log", b"hello\n"), entry(b"b.log", HIDDEN)], &[0, 1], b"", None, 0);
    let (out, err, c) = search("two.zip", &two);
    assert!(out.contains("b.log:1:secret NEEDLE line"), "{out} / {err}");
    assert_eq!((err.as_str(), c), ("", Some(0)));
}

/// `cat a.zip b.zip`: the last zip's directory is the one at the end, and
/// the first zip is bytes in front of it.
#[test]
fn a_zip_behind_another_zip_is_two() {
    let first = helpers::zip_bytes(&[("first.log", b"NEEDLE in the first zip\n")], ZipMethod::Deflated);
    let second = helpers::zip_bytes(&[("second.log", b"NEEDLE in the second zip\n")], ZipMethod::Deflated);
    let (out, err, c) = search("ab.zip", &[first.clone(), second].concat());
    assert!(out.contains("second.log:1:NEEDLE in the second zip"), "{out} / {err}");
    assert_eq!(c, Some(2), "{err}");
    assert!(err.contains(&format!("{} bytes before the zip's first entry were not searched", first.len())), "{err}");
}

/// A self-extracting zip, or one with a script in front: not a zip by its
/// first bytes, and it was searched as a plain file.
#[test]
fn a_zip_behind_a_script_is_read_as_a_zip_and_is_two() {
    let stub = b"#!/bin/sh\necho self-extracting\nexit 0\n";
    let zip = helpers::zip_bytes(&[("logs/a.log", b"one\nNEEDLE deflated\n")], ZipMethod::Deflated);
    let (out, err, c) = search("sfx.zip", &[&stub[..], &zip].concat());
    assert!(out.contains("logs/a.log:2:NEEDLE deflated"), "{out} / {err}");
    assert_eq!(c, Some(2), "{err}");
    assert!(err.contains(&format!("{} bytes before the zip's first entry", stub.len())), "{err}");

    // With its offsets adjusted to the stub, as `zip -A` leaves them.
    let adjusted = build(&[entry(b"a.log", HIDDEN)], &[0], b"", None, stub.len() as u32);
    let (out, err, c) = search("sfx-adjusted.zip", &[&stub[..], &adjusted].concat());
    assert!(out.contains("a.log:1:secret NEEDLE line"), "{out} / {err}");
    assert_eq!(c, Some(2), "{err}");
    assert!(err.contains("before the zip's first entry"), "{err}");

    // And an empty zip behind a stub is still the stub's bytes unread.
    let empty = helpers::zip_bytes(&[], ZipMethod::Deflated);
    let (_, err, c) = search("sfx-empty.zip", &[&stub[..], &empty].concat());
    assert_eq!(c, Some(2), "{err}");
}

/// Text that merely mentions a zip's signatures is still text.
#[test]
fn a_plain_file_is_not_taken_for_a_zip_by_its_tail() {
    let mut log = b"NEEDLE in a log\n".to_vec();
    log.extend_from_slice(b"it ends in PK\x05\x06 and a few more bytes\n");
    let (out, err, c) = search("x.log", &log);
    assert!(out.contains("NEEDLE in a log"), "{out} / {err}");
    assert_eq!((err.as_str(), c), ("", Some(0)));

    // An end record alone at the end, leading to no directory.
    let mut log = b"NEEDLE in a log\n".to_vec();
    log.extend(end_record(1, 46, 0, b""));
    let (out, _, c) = search("y.log", &log);
    assert!(out.contains("NEEDLE in a log") || out.contains("binary file matches"), "{out}");
    assert_eq!(c, Some(0));
}

/// The end record counts one entry and the directory holds two: unzip,
/// bsdtar and Python read both.
#[test]
fn an_end_record_that_counts_too_few_entries_is_two() {
    let z = build(&[entry(b"a.log", b"hello\n"), entry(b"hidden.log", HIDDEN)], &[0, 1], b"", Some(1), 0);
    let (out, err, c) = search("count.zip", &z);
    assert_eq!(c, Some(2), "{out} / {err}");
    assert!(err.contains("lists 2 entries and 1 were read"), "{err}");
}

/// An entry whose name ends in `/`, or whose mode is a symlink's, with a
/// log in it: `unzip -p` prints it.
#[test]
fn a_directory_or_symlink_entry_with_data_is_searched() {
    let dir = build(&[entry(b"a.log", b"hello\n"), entry(b"hidden.log/", HIDDEN)], &[0, 1], b"", None, 0);
    let (out, err, c) = search("dir.zip", &dir);
    assert!(out.contains("hidden.log/:1:secret NEEDLE line"), "{out} / {err}");
    assert_eq!((err.as_str(), c), ("", Some(0)));

    let link = Entry { attrs: 0o120777 << 16, made_by: (3 << 8) | 20, ..entry(b"hidden.log", HIDDEN) };
    let z = build(&[entry(b"a.log", b"hello\n"), link], &[0, 1], b"", None, 0);
    let (out, err, c) = search("link.zip", &z);
    assert!(out.contains("hidden.log:1:secret NEEDLE line"), "{out} / {err}");
    assert_eq!((err.as_str(), c), ("", Some(0)));
}

/// Directories that are only that are still not members.
#[test]
fn an_empty_directory_entry_is_still_not_a_member() {
    let f = helpers::zip("t.zip", &[("logs/a.log", b"x\n"), ("logs/deep/b.log", b"y\n")]);
    let p = f.path.to_str().unwrap();
    let (out, err, c) = run_trg(&["-T", p]);
    assert_eq!(out, format!("{p}:logs/a.log\n{p}:logs/deep/b.log\n"), "{err}");
    assert_eq!((err.as_str(), c), ("", Some(0)));
}

#[test]
fn data_after_the_end_record_is_two_and_zeros_are_padding() {
    let mut z = plain();
    z.extend_from_slice(b"\nNEEDLE after the zip\n");
    let (_, err, c) = search("trailing.zip", &z);
    assert_eq!(c, Some(2), "{err}");
    assert!(err.contains("22 bytes after the zip's end record were not searched"), "{err}");

    for pad in [1, 512, 10_240] {
        let mut z = plain();
        z.resize(z.len() + pad, 0);
        let (_, err, c) = search("padded.zip", &z);
        assert_eq!((err.as_str(), c), ("", Some(1)), "{pad} zeros");
        z.push(b'x');
        let (_, err, c) = search("padded-then-data.zip", &z);
        assert_eq!(c, Some(2), "{pad} zeros and a byte: {err}");
    }
}

/// An entry the directory does not list, data between the last entry and
/// the directory, and data past the size the directory gives an entry.
#[test]
fn data_between_the_entries_that_no_entry_names_is_two() {
    let a = entry(b"a.log", b"hello\n");
    let unlisted = build(&[a.clone(), entry(b"hidden.log", HIDDEN)], &[0], b"", None, 0);
    let gap = build(std::slice::from_ref(&a), &[0], b"\nNEEDLE in the gap\n", None, 0);
    let short = Entry { listed: Some(b"visible line\n"), ..entry(b"big.log", b"visible line\nNEEDLE past the stated size\n") };
    let short = build(&[a.clone(), short], &[0, 1], b"", None, 0);
    for (label, z) in [("unlisted entry", unlisted), ("gap", gap), ("short size", short)] {
        let (_, err, c) = search("x.zip", &z);
        assert_eq!(c, Some(2), "{label}: {err}");
        assert!(err.contains("are in no entry the zip's directory lists: not searched"), "{label}: {err}");
    }

    let zeros = build(&[a], &[0], &[0; 700], None, 0);
    let (_, err, c) = search("zero-gap.zip", &zeros);
    assert_eq!((err.as_str(), c), ("", Some(1)), "a gap of zeros");
}

/// A whole zip, then a second directory and end record that list only its
/// first entry.
#[test]
fn a_second_end_record_that_lists_less_is_two() {
    let a = entry(b"a.log", b"hello\n");
    let mut z = build(&[a.clone(), entry(b"hidden.log", HIDDEN)], &[0, 1], b"", None, 0);
    let dir = central(&a, 0);
    let at = z.len() as u32;
    z.extend_from_slice(&dir);
    z.extend(end_record(1, dir.len() as u32, at, b""));
    let (_, err, c) = search("two-ends.zip", &z);
    assert_eq!(c, Some(2), "{err}");
}

/// What a healthy zip holds besides its entries' data must not be taken
/// for unread bytes: data descriptors, a comment, the split marker.
#[test]
fn descriptors_comments_and_the_split_marker_are_not_unread_data() {
    let described = Entry { described: true, ..entry(b"a.log", b"hello\n") };
    let also = Entry { described: true, ..entry(b"b.log", b"more\n") };
    let z = build(&[described, also, entry(b"c.log", b"x\n")], &[0, 1, 2], b"", None, 0);
    let (_, err, c) = search("described.zip", &z);
    assert_eq!((err.as_str(), c), ("", Some(1)), "data descriptors");

    let mut z = plain();
    let n = z.len();
    z[n - 2] = 9;
    z.extend_from_slice(b"a comment");
    let (_, err, c) = search("comment.zip", &z);
    assert_eq!((err.as_str(), c), ("", Some(1)), "an archive comment");

    let split = [b"PK\x07\x08".to_vec(), build(&[entry(b"a.log", b"hello\n")], &[0], b"", None, 4)].concat();
    let (_, err, c) = search("split.zip", &split);
    assert_eq!((err.as_str(), c), ("", Some(1)), "the split marker");
}

/// Every method the zip crate writes, streamed (data descriptors) and not,
/// and zip64 records.
#[test]
fn zips_written_by_the_zip_crate_are_clean_reads() {
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    for method in [ZipMethod::Stored, ZipMethod::Deflated, ZipMethod::Bzip2] {
        let z = helpers::zip_bytes(&[("logs/a.log", b"x\n"), ("logs/deep/b.log", b"y\n")], method);
        let (_, err, c) = search("crate.zip", &z);
        assert_eq!((err.as_str(), c), ("", Some(1)));
    }

    let mut w = zip::ZipWriter::new_stream(Vec::new());
    for name in ["a.log", "b.log"] {
        w.start_file(name, SimpleFileOptions::default()).unwrap();
        w.write_all(b"streamed\n").unwrap();
    }
    let z = w.finish().unwrap().into_inner();
    let (_, err, c) = search("streamed.zip", &z);
    assert_eq!((err.as_str(), c), ("", Some(1)), "streamed");

    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for name in ["a.log", "b.log"] {
        w.start_file(name, SimpleFileOptions::default().large_file(true)).unwrap();
        w.write_all(b"zip64\n").unwrap();
    }
    let z = w.finish().unwrap().into_inner();
    let (_, err, c) = search("zip64.zip", &z);
    assert_eq!((err.as_str(), c), ("", Some(1)), "zip64 entries");
}

/// The tools that write most zips, where they are installed: Info-ZIP's
/// `zip` (plain, from a pipe, forced zip64, grown and deleted from, with a
/// comment) and bsdtar.
#[test]
fn zips_written_by_zip_and_bsdtar_are_clean_reads() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::write(d.join("a.log"), "one\ntwo\n").unwrap();
    std::fs::write(d.join("b.log"), "x\n".repeat(5000)).unwrap();
    std::fs::create_dir_all(d.join("sub/deep")).unwrap();
    std::fs::write(d.join("sub/deep/c.log"), "three\n").unwrap();
    let sh = |script: &str| {
        Command::new("sh").arg("-c").arg(script).current_dir(d).output().is_ok_and(|o| o.status.success())
    };
    let made = [
        ("zip -q -r plain.zip a.log b.log sub", "plain.zip"),
        ("zip -q -0 stored.zip a.log b.log", "stored.zip"),
        ("cat b.log | zip -q piped.zip -", "piped.zip"),
        ("zip -q -fz z64.zip a.log b.log", "z64.zip"),
        ("zip -q grown.zip a.log && zip -q grown.zip b.log && zip -q -d grown.zip a.log", "grown.zip"),
        ("zip -q noted.zip a.log && echo a comment | zip -q -z noted.zip", "noted.zip"),
        ("bsdtar -a -cf bsd.zip a.log b.log sub", "bsd.zip"),
    ];
    for (script, name) in made {
        if !sh(script) {
            eprintln!("skipped, could not run: {script}");
            continue;
        }
        let (_, err, c) = run_trg(&["NEEDLE", d.join(name).to_str().unwrap()]);
        assert_eq!((err.as_str(), c), ("", Some(1)), "{script}");
    }
}
