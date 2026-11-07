//! A checksum that is never read checks nothing. Every format here carries
//! one at the end of its stream, or of each member, and a run that stops
//! reading before it reaches it can call a corrupt or cut-off archive clean.

mod helpers;

use std::process::Command;

fn run(args: &[&str]) -> (String, String, Option<i32>) {
    let o = Command::new(env!("CARGO_BIN_EXE_trg")).args(args).output().unwrap();
    (String::from_utf8_lossy(&o.stdout).into_owned(),
     String::from_utf8_lossy(&o.stderr).into_owned(),
     o.status.code())
}

fn log(lines: usize) -> Vec<u8> {
    (0..lines).map(|i| format!("10.0.0.{} GET /page/{i} 200\n", i % 250)).collect::<String>().into_bytes()
}

/// Where each lz4 block ends, after the frame header: the offsets a cut can
/// fall on without landing inside a block.
fn lz4_block_ends(d: &[u8]) -> Vec<usize> {
    let flg = d[4];
    let mut at = 7 + if flg & 0x08 != 0 { 8 } else { 0 } + if flg & 0x01 != 0 { 4 } else { 0 };
    let block_sum = if flg & 0x10 != 0 { 4 } else { 0 };
    let mut ends = vec![at];
    loop {
        let n = u32::from_le_bytes(d[at..at + 4].try_into().unwrap());
        if n == 0 {
            return ends;
        }
        at += 4 + (n & 0x7fff_ffff) as usize + block_sum;
        ends.push(at);
    }
}

/// lz4_flex takes the end of the input where a block size should be for the
/// end of the stream, frame finished or not. A cut there must still be a 2.
#[test]
fn an_lz4_cut_between_blocks_is_two() {
    let full = {
        use std::io::Write;
        let info = lz4_flex::frame::FrameInfo::new()
            .block_size(lz4_flex::frame::BlockSize::Max64KB)
            .content_checksum(true);
        let mut e = lz4_flex::frame::FrameEncoder::with_frame_info(info, Vec::new());
        e.write_all(&log(40_000)).unwrap();
        e.finish().unwrap()
    };
    let ends = lz4_block_ends(&full);
    assert!(ends.len() > 3, "needs several blocks: {}", ends.len());
    for &cut in &ends[..ends.len() - 1] {
        let f = helpers::raw("a.log.lz4", &full[..cut]);
        let (out, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
        assert_eq!(c, Some(2), "cut at {cut} of {}: {out} / {err}", full.len());
    }
    // Inside a tar, as an rotated member, too.
    let f = helpers::tgz("m.tgz", &[("logs/a.log.1.lz4", &full[..ends[2]])]);
    let (_, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(2), "member cut between blocks: {err}");
}

/// Two whole frames back to back are one valid file, which the frame-aware
/// cut check must not mistake for a cut.
#[test]
fn concatenated_lz4_frames_are_still_whole() {
    let mut two = helpers::lz4_bytes(b"first frame\n");
    two.extend(helpers::lz4_bytes(b"NEEDLE second frame\n"));
    let f = helpers::raw("two.lz4", &two);
    let (out, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert!(out.contains(":2:NEEDLE second frame"), "got: {out} / {err}");
    assert_eq!(c, Some(0), "got: {err}");
    let (_, err, c) = run(&["ZZZ", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(1), "a whole file with no match is a 1: {err}");
}

/// The gzip trailer — CRC-32 and length — comes after the tar's
/// end-of-archive blocks. Stopping at those blocks never read it.
#[test]
fn a_tgz_with_a_bad_trailer_is_two() {
    let f = helpers::tgz("x.tgz", &[("logs/a.log", &log(2000))]);
    let mut d = std::fs::read(&f.path).unwrap();
    let n = d.len();
    d[n - 8] ^= 0xff;
    let f = helpers::raw("x.tgz", &d);
    let (_, err, c) = run(&["ZZZ", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(2), "no match, but the CRC was never checked: {err}");
    let (out, err, c) = run(&["10.0.0.7 ", f.path.to_str().unwrap()]);
    assert!(!out.is_empty());
    assert_eq!(c, Some(2), "matches, but not everything checked out: {err}");
}

/// Same for bzip2, whose stream CRC sits after its last block.
#[test]
fn a_tar_bz2_with_a_bad_stream_crc_is_two() {
    let full = helpers::bz2_bytes(&helpers::tar_bytes(&[("logs/a.log", &log(2000))]));
    let mut d = full.clone();
    let n = d.len();
    d[n - 2] ^= 0xff;
    let f = helpers::raw("x.tar.bz2", &d);
    let (_, err, c) = run(&["ZZZ", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(2), "got: {err}");
}

/// A member that looks binary stops being searched at its first NUL, which
/// left the rest of it, and the checksum at its end, unread.
#[test]
fn a_binary_looking_member_is_still_read_to_its_checksum() {
    let mut body = b"header\0binary from here\n".to_vec();
    body.extend(log(5000));
    let mut gz = {
        use std::io::Write;
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&body).unwrap();
        e.finish().unwrap()
    };
    let n = gz.len();
    gz[n - 8] ^= 0xff;
    let f = helpers::raw("core.gz", &gz);
    let (_, err, c) = run(&["ZZZ", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(2), "a whole file: {err}");

    let t = helpers::tar_plain("t.tar", &[("logs/core.1.gz", &gz)]);
    let (_, err, c) = run(&["ZZZ", t.path.to_str().unwrap()]);
    assert_eq!(c, Some(2), "a member: {err}");
}

/// An endless stream is read until it is stopped, as rg and grep read
/// /dev/zero: a 1 would claim it was read to its end. Every NUL ends a line,
/// so memory stays flat however long it runs.
#[cfg(unix)]
#[test]
fn an_endless_binary_stream_is_read_in_flat_memory_until_stopped() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_trg"))
        .args(["NEEDLE", "/dev/zero"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_secs(2));
    assert!(child.try_wait().unwrap().is_none(), "trg stopped reading /dev/zero");
    let ps = Command::new("ps").args(["-o", "rss=", "-p", &child.id().to_string()]).output().unwrap();
    child.kill().unwrap();
    let _ = child.wait();
    let kib: u64 = String::from_utf8_lossy(&ps.stdout).trim().parse().unwrap();
    assert!(kib < 64 * 1024, "{kib} KiB after 2 s of /dev/zero");
}

/// A stored zip entry has no decoder, but it has a CRC-32 at its end.
#[test]
fn a_binary_looking_stored_zip_entry_is_read_to_its_crc() {
    let mut body = b"header\0binary from here\n".to_vec();
    body.extend(log(2000));
    let mut z = helpers::zip_bytes(&[("core.1", &body)], helpers::ZipMethod::Stored);
    // The stored data starts after the 30-byte local header and the name.
    z[30 + 6 + 100] ^= 0x01;
    let f = helpers::raw("c.zip", &z);
    let (_, err, c) = run(&["ZZZ", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(2), "got: {err}");
}

/// A plain tar has no checksum, and its reader takes the end of the input at
/// a header boundary for the end of the archive. A cut between two members
/// must still be a 2, and only the zero blocks every tar ends in tell the two
/// apart.
#[test]
fn a_plain_tar_cut_between_members_is_two() {
    let full = helpers::tar_bytes(&[("logs/b.log", &log(100)), ("logs/a.log", b"NEEDLE\n")]);
    let second = 512 + log(100).len().div_ceil(512) * 512;
    let f = helpers::raw("cut.tar", &full[..second]);
    let (_, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(2), "cut at {second} of {}: {err}", full.len());
    assert!(err.contains("end-of-archive"), "got: {err}");
    let f = helpers::raw("whole.tar", &full);
    let (_, err, c) = run(&["NEEDLE", f.path.to_str().unwrap()]);
    assert_eq!(c, Some(0), "the whole tar: {err}");
}

/// bsdtar, macOS's tar, writes `tar czf - dir > x.tgz` in 10 KiB blocks
/// and fills the last with zeros. Every format's decoder read the zeros as a
/// broken next stream, a 2 on an archive tar reads whole.
#[test]
fn zero_padding_after_the_compressed_stream_is_not_an_error() {
    let tar = helpers::tar_bytes(&[("logs/a.log", b"x\nERR one\n")]);
    let formats: [(&str, Vec<u8>); 5] = [
        ("tgz", helpers::gzip_bytes(&tar)),
        ("tar.bz2", helpers::bz2_bytes(&tar)),
        ("tar.zst", helpers::zstd_plain_bytes(&tar)),
        ("tar.lz4", helpers::lz4_bytes(&tar)),
        ("tar.xz", helpers::xz_bytes(&tar)),
    ];
    for (ext, bytes) in formats {
        let f = helpers::raw(&format!("p.{ext}"), &helpers::padded(&bytes, 10240));
        let p = f.path.to_str().unwrap();
        let (out, err, c) = helpers::run_trg(&["ERR", p]);
        assert!(out.ends_with("logs/a.log:2:ERR one\n"), "{ext}: {out}");
        assert!(err.is_empty(), "{ext}: {err}");
        assert_eq!(c, Some(0), "{ext}");
        let (_, err, c) = helpers::run_trg(&["NOPE", p]);
        assert!(err.is_empty(), "{ext}: {err}");
        assert_eq!(c, Some(1), "{ext}");

        // Zeros, then something that is not a stream: still a 2.
        let mut junk = helpers::padded(&bytes, 10240);
        junk.extend_from_slice(b"junk");
        let f = helpers::raw(&format!("j.{ext}"), &junk);
        let (_, _, c) = helpers::run_trg(&["NOPE", f.path.to_str().unwrap()]);
        assert_eq!(c, Some(2), "{ext}: junk after the padding");
    }
}

/// `cat day1.tgz day2.tgz > week.tgz` is one valid gzip holding two tars.
/// Stopping at the first tar's end left the second unsearched: a 1 about a
/// day never read. Read on, as `tar -i` does, in every format.
#[test]
fn concatenated_archives_are_all_searched() {
    let one = helpers::tar_bytes(&[("day1.log", b"first ERR\n")]);
    let two = helpers::tar_bytes(&[("day2.log", b"second FATAL\n")]);
    let cat = |f: fn(&[u8]) -> Vec<u8>| [f(&one), f(&two)].concat();
    let formats: [(&str, Vec<u8>); 6] = [
        ("tar", [one.clone(), two.clone()].concat()),
        ("tgz", cat(helpers::gzip_bytes)),
        ("tar.bz2", cat(helpers::bz2_bytes)),
        ("tar.zst", cat(helpers::zstd_plain_bytes)),
        ("tar.lz4", cat(helpers::lz4_bytes)),
        ("tar.xz", cat(helpers::xz_bytes)),
    ];
    for (ext, bytes) in formats {
        let f = helpers::raw(&format!("week.{ext}"), &bytes);
        let p = f.path.to_str().unwrap();
        let (out, err, c) = helpers::run_trg(&["FATAL", p]);
        assert!(out.ends_with("day2.log:1:second FATAL\n"), "{ext}: {out}");
        assert!(err.is_empty(), "{ext}: {err}");
        assert_eq!(c, Some(0), "{ext}");
        let (out, _, c) = helpers::run_trg(&["-T", p]);
        assert_eq!(out.lines().count(), 2, "{ext}: {out}");
        assert_eq!(c, Some(0), "{ext}");
    }
}

/// What follows a tar's end must be zeros or another tar. Anything else was
/// read and thrown away; it may be the rest of something.
#[test]
fn bytes_after_a_tars_end_that_are_not_a_tar_are_a_two() {
    let mut t = helpers::tar_bytes(&[("a.log", b"x\n")]);
    t.extend(std::iter::repeat_n(b'j', 2048));
    for (ext, bytes) in [("tar", t.clone()), ("tgz", helpers::gzip_bytes(&t))] {
        let f = helpers::raw(&format!("j.{ext}"), &bytes);
        let (_, err, c) = helpers::run_trg(&["NOPE", f.path.to_str().unwrap()]);
        assert!(!err.is_empty(), "{ext}");
        assert_eq!(c, Some(2), "{ext}");
    }
}
