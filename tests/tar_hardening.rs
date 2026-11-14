//! Tars that are not the plain ustar a test fixture writes: other entry
//! types, sparse members, extension headers, cuts and padding, and the
//! formats around them that trg cannot read. Each was a 1 about bytes never
//! searched, or a 2 about an archive tar reads whole.

mod helpers;

use helpers::run_trg;
use std::io::Write;

const USTAR: &[u8; 8] = b"ustar\x0000";
const GNU: &[u8; 8] = b"ustar  \0";

fn octal(field: &mut [u8], v: u64) {
    let s = format!("{:0w$o}\0", v, w = field.len() - 1);
    field.copy_from_slice(s.as_bytes());
}

/// Write the header's checksum, the last thing done to it.
fn seal(h: &mut [u8]) {
    h[148..156].fill(b' ');
    let sum: u32 = h[..512].iter().map(|&b| b as u32).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
}

fn header(name: &str, flag: u8, size: u64, magic: Option<&[u8; 8]>) -> Vec<u8> {
    let mut h = vec![0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    octal(&mut h[100..108], 0o644);
    octal(&mut h[108..116], 0);
    octal(&mut h[116..124], 0);
    octal(&mut h[124..136], size);
    octal(&mut h[136..148], 1_756_000_000);
    h[156] = flag;
    if let Some(m) = magic {
        h[257..265].copy_from_slice(m);
    }
    seal(&mut h);
    h
}

/// `body`, filled to a whole block.
fn blocks(body: &[u8]) -> Vec<u8> {
    let mut v = body.to_vec();
    v.resize(body.len().div_ceil(512) * 512, 0);
    v
}

fn member(name: &str, flag: u8, body: &[u8]) -> Vec<u8> {
    let mut v = header(name, flag, body.len() as u64, Some(USTAR));
    v.extend(blocks(body));
    v
}

fn end() -> Vec<u8> {
    vec![0u8; 1024]
}

fn tar(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut v = parts.concat();
    v.extend(end());
    v
}

fn pax_record(key: &str, value: &str) -> String {
    let body = format!(" {key}={value}\n");
    let mut len = body.len() + 1;
    while format!("{len}").len() + body.len() != len {
        len += 1;
    }
    format!("{len}{body}")
}

fn pax(records: &[(&str, &str)]) -> Vec<u8> {
    let data: String = records.iter().map(|(k, v)| pax_record(k, v)).collect();
    member("./PaxHeaders.1/app.log", b'x', data.as_bytes())
}

fn search(pattern: &str, name: &str, bytes: &[u8]) -> (String, String, Option<i32>) {
    let f = helpers::raw(name, bytes);
    // -a: a sparse member has its holes, which are NULs.
    run_trg(&["-an", pattern, f.path.to_str().unwrap()])
}

fn list(name: &str, bytes: &[u8]) -> String {
    let f = helpers::raw(name, bytes);
    run_trg(&["-T", f.path.to_str().unwrap()]).0
}

// ---- 1. entry types ------------------------------------------------------

/// Only typeflag '0' was searched. A contiguous file ('7'), a vendor or
/// unknown letter and a hard link that carries data all hold bytes, and
/// leaving them out was a 1 about them.
#[test]
fn content_under_any_typeflag_is_searched_and_listed() {
    for flag in *b"7ZADM1\0" {
        let t = tar(&[member("logs/a.log", flag, b"x\nNEEDLE here\n"), member("logs/b.log", b'0', b"y\n")]);
        let (out, err, c) = search("NEEDLE", "t.tar", &t);
        let flag = flag as char;
        assert!(out.contains("logs/a.log:2:NEEDLE here"), "{flag:?}: {out} / {err}");
        assert_eq!(c, Some(0), "{flag:?}: {err}");
        let (_, err, c) = search("NOPE", "t.tar", &t);
        assert_eq!((c, err.as_str()), (Some(1), ""), "{flag:?}");
        let listed = list("t.tar", &t);
        assert!(listed.contains("logs/a.log"), "{flag:?}: {listed}");
    }
}

/// Entries with no content of their own are not members.
#[test]
fn directories_links_and_devices_are_not_members() {
    let mut link = header("logs/link", b'2', 0, Some(USTAR));
    link[157..165].copy_from_slice(b"a.log\0\0\0");
    seal(&mut link);
    let t = tar(&[
        header("logs/", b'5', 0, Some(USTAR)),
        link,
        header("logs/hard", b'1', 0, Some(USTAR)),
        header("logs/tty", b'3', 0, Some(USTAR)),
        header("logs/disk", b'4', 0, Some(USTAR)),
        header("logs/pipe", b'6', 0, Some(USTAR)),
        member("logs/a.log", b'0', b"NEEDLE\n"),
    ]);
    let listed = list("t.tar", &t);
    assert_eq!(listed.lines().count(), 1, "{listed}");
    assert!(listed.contains("logs/a.log"), "{listed}");
    let (_, err, c) = search("NOPE", "t.tar", &t);
    assert_eq!((c, err.as_str()), (Some(1), ""));
}

/// Old tars give a directory or a hard link the size of what it names and
/// no data: GNU tar, Python and Go all read them so. The next block is then
/// a header, and reading it as the entry's data lost the member it starts.
#[test]
fn a_sized_directory_or_link_without_data_does_not_swallow_the_next_member() {
    for flag in *b"512" {
        let t = tar(&[header("logs/dir", flag, 512, Some(USTAR)), member("logs/a.log", b'0', b"NEEDLE\n")]);
        let (out, err, c) = search("NEEDLE", "t.tar", &t);
        assert!(out.contains("logs/a.log:1:NEEDLE"), "{}: {out} / {err}", flag as char);
        assert_eq!((c, err.as_str()), (Some(0), ""));
    }
}

/// The same entries, when data does follow them: searched, not skipped.
#[test]
fn a_directory_entry_with_data_is_searched() {
    for flag in *b"52346" {
        let t = tar(&[member("logs/odd", flag, b"x\nNEEDLE hidden\n")]);
        let (out, err, c) = search("NEEDLE", "t.tar", &t);
        assert!(out.contains("logs/odd:2:NEEDLE hidden"), "{}: {out} / {err}", flag as char);
        assert_eq!(c, Some(0));
    }
}

/// An old-GNU sparse header ('S', `gtar -S`): four map entries in the
/// header and, here, more in an extension block. The data on the archive is
/// the segments end to end.
fn old_gnu_sparse(name: &str, segments: &[(u64, &[u8])], realsize: u64) -> Vec<u8> {
    let stored: Vec<u8> = segments.iter().flat_map(|(_, d)| d.to_vec()).collect();
    let mut h = header(name, b'S', stored.len() as u64, Some(GNU));
    for (i, (off, d)) in segments.iter().take(4).enumerate() {
        let at = 386 + i * 24;
        octal(&mut h[at..at + 12], *off);
        octal(&mut h[at + 12..at + 24], d.len() as u64);
    }
    octal(&mut h[483..495], realsize);
    let more = &segments[segments.len().min(4)..];
    h[482] = !more.is_empty() as u8;
    seal(&mut h);
    let mut v = h;
    let chunks: Vec<_> = more.chunks(21).collect();
    for (n, chunk) in chunks.iter().enumerate() {
        let mut x = vec![0u8; 512];
        for (i, (off, d)) in chunk.iter().enumerate() {
            octal(&mut x[i * 24..i * 24 + 12], *off);
            octal(&mut x[i * 24 + 12..i * 24 + 24], d.len() as u64);
        }
        x[504] = (n + 1 < chunks.len()) as u8;
        v.extend(x);
    }
    v.extend(blocks(&stored));
    v
}

#[test]
fn an_old_gnu_sparse_member_is_searched_expanded() {
    let lines: Vec<Vec<u8>> = (0..30).map(|i| format!("\nNEEDLE {i}\n").into_bytes()).collect();
    let segments: Vec<(u64, &[u8])> =
        lines.iter().enumerate().map(|(i, l)| (i as u64 * 8192, l.as_slice())).collect();
    for count in [2, 4, 5, 30] {
        let t = tar(&[
            old_gnu_sparse("logs/sparse.log", &segments[..count], 30 * 8192),
            member("logs/after.log", b'0', b"NEEDLE after\n"),
        ]);
        let (out, err, c) = search("NEEDLE", "t.tar", &t);
        assert_eq!(c, Some(0), "{count}: {err}");
        for i in 0..count {
            let want = format!("logs/sparse.log:{}:NEEDLE {i}\n", 2 + 2 * i);
            assert!(out.contains(&want), "{count} segments, want {want:?}: {out} / {err}");
        }
        assert!(out.contains("logs/after.log:1:NEEDLE after"), "{count}: {out} / {err}");
        assert!(err.is_empty(), "{count}: {err}");
        // The holes are there: the member is its real size.
        let f = helpers::raw("t.tar", &t);
        let (json, _, _) = run_trg(&["-a", "--json", "NEEDLE 1$", f.path.to_str().unwrap()]);
        assert!(json.contains("\"absolute_offset\":8193,"), "{count}: {json}");
    }
}

// ---- 2. pax sparse -------------------------------------------------------

const SPARSE_DATA: &[u8] = b"first\n\nNEEDLE two\n";

/// Format 1.0, what bsdtar and `gtar -S --format=posix` write: the map is
/// decimal text at the start of the data, and the real name is in the pax
/// header. The map was searched as content and the member was named
/// `GNUSparseFile.0/app.log`.
#[test]
fn a_pax_sparse_1_0_member_has_its_name_and_no_map_in_its_content() {
    let mut body = blocks(b"2\n0\n6\n4096\n12\n");
    body.extend_from_slice(SPARSE_DATA);
    let t = tar(&[
        pax(&[
            ("GNU.sparse.major", "1"),
            ("GNU.sparse.minor", "0"),
            ("GNU.sparse.name", "logs/app.log"),
            ("GNU.sparse.realsize", "8192"),
        ]),
        member("logs/GNUSparseFile.0/app.log", b'0', &body),
        member("logs/after.log", b'0', b"4096 after\n"),
    ]);
    let (out, err, c) = search("NEEDLE", "t.tar", &t);
    assert!(out.contains("logs/app.log:3:NEEDLE two"), "{out} / {err}");
    assert!(!out.contains("GNUSparseFile"), "{out}");
    assert_eq!((c, err.as_str()), (Some(0), ""));
    let (out, err, _) = search("4096", "t.tar", &t);
    assert!(!out.contains("app.log"), "the map is not content: {out} / {err}");
    assert!(out.contains("logs/after.log:1:4096 after"), "{out} / {err}");
    let listed = list("t.tar", &t);
    assert!(listed.contains("logs/app.log") && !listed.contains("GNUSparseFile"), "{listed}");
    // Expanded: the second segment is at its real offset.
    let f = helpers::raw("t.tar", &t);
    let (json, _, _) = run_trg(&["-a", "--json", "NEEDLE", f.path.to_str().unwrap()]);
    assert!(json.contains("\"absolute_offset\":4097,"), "{json}");
}

/// Formats 0.0 and 0.1 keep the map in the pax records.
#[test]
fn pax_sparse_0_0_and_0_1_members_are_expanded() {
    let v01 = pax(&[
        ("GNU.sparse.size", "8192"),
        ("GNU.sparse.numblocks", "2"),
        ("GNU.sparse.name", "logs/app.log"),
        ("GNU.sparse.map", "0,6,4096,12"),
    ]);
    let v00 = pax(&[
        ("path", "logs/app.log"),
        ("GNU.sparse.size", "8192"),
        ("GNU.sparse.numblocks", "2"),
        ("GNU.sparse.offset", "0"),
        ("GNU.sparse.numbytes", "6"),
        ("GNU.sparse.offset", "4096"),
        ("GNU.sparse.numbytes", "12"),
    ]);
    for (what, x) in [("0.1", v01), ("0.0", v00)] {
        let t = tar(&[x, member("logs/GNUSparseFile.0/app.log", b'0', SPARSE_DATA)]);
        let (out, err, c) = search("NEEDLE", "t.tar", &t);
        assert!(out.contains("logs/app.log:3:NEEDLE two"), "{what}: {out} / {err}");
        assert_eq!((c, err.as_str()), (Some(0), ""), "{what}");
        let f = helpers::raw("t.tar", &t);
        let (json, _, _) = run_trg(&["-a", "--json", "NEEDLE", f.path.to_str().unwrap()]);
        assert!(json.contains("\"absolute_offset\":4097,"), "{what}: {json}");
    }
}

/// A map that does not add up to the data stored is not guessed at.
#[test]
fn a_sparse_map_that_does_not_match_the_data_is_a_two() {
    let x = pax(&[("GNU.sparse.size", "8192"), ("GNU.sparse.name", "logs/app.log"), ("GNU.sparse.map", "0,6,4096,999")]);
    let t = tar(&[x, member("logs/GNUSparseFile.0/app.log", b'0', SPARSE_DATA)]);
    let (_, err, c) = search("NOPE", "t.tar", &t);
    assert_eq!(c, Some(2), "{err}");
    assert!(err.contains("sparse"), "{err}");
}

// ---- 3. cut at a header boundary ----------------------------------------

/// A member whose data ends in a block of zeros, and a cut right after it:
/// the zeros were counted as the end-of-archive blocks.
#[test]
fn a_cut_after_a_member_ending_in_zeros_is_a_two() {
    let mut a = b"first line\n".to_vec();
    a.extend(std::iter::repeat_n(0u8, 1024));
    let full = helpers::tar_bytes(&[("a.log", &a), ("hit.log", b"ccc\nNEEDLE x\n")]);
    let (_, err, c) = search("NEEDLE", "cut.tar", &full[..2048]);
    assert_eq!(c, Some(2), "{err}");
    assert!(err.contains("end-of-archive"), "{err}");
    let (_, _, c) = search("NEEDLE", "cut.tgz", &helpers::gzip_bytes(&full[..2048]));
    assert_eq!(c, Some(2), "inside a gzip");
    let (_, err, c) = search("NEEDLE", "full.tar", &full);
    assert_eq!(c, Some(0), "{err}");
}

// ---- 4. extension headers of any size -----------------------------------

/// The peak resident set of `trg NEEDLE path`, in KiB, its exit code and
/// its stderr.
#[cfg(unix)]
fn peak_rss(path: &std::path::Path) -> (u64, Option<i32>, String) {
    use std::process::{Command, Stdio};
    let mut child = Command::new(env!("CARGO_BIN_EXE_trg"))
        .arg("NEEDLE")
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = child.stderr.take().unwrap();
    // Read from a thread: a gigabyte of name on stderr would fill the pipe.
    let reader = std::thread::spawn(move || {
        let mut err = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stderr, &mut err);
        err
    });
    let mut peak = 0u64;
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        let ps = Command::new("ps").args(["-o", "rss=", "-p", &child.id().to_string()]).output().unwrap();
        if let Ok(kib) = String::from_utf8_lossy(&ps.stdout).trim().parse::<u64>() {
            peak = peak.max(kib);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let err = reader.join().unwrap();
    (peak, status.code(), String::from_utf8_lossy(&err).into_owned())
}

/// A long-name, long-link or pax header that declares a gigabyte: the name
/// was read whole into memory before anything looked at it, megabytes of
/// archive for gigabytes of memory.
#[cfg(unix)]
#[test]
fn a_huge_extension_header_is_refused_in_flat_memory() {
    let dir = tempfile::tempdir().unwrap();
    for flag in *b"LKxg" {
        let path = dir.path().join(format!("bomb-{}.tgz", flag as char));
        let mut gz = flate2::write::GzEncoder::new(
            std::io::BufWriter::new(std::fs::File::create(&path).unwrap()),
            flate2::Compression::fast(),
        );
        gz.write_all(&header("././@LongLink", flag, 1 << 30, Some(GNU))).unwrap();
        let chunk = vec![b'a'; 1 << 20];
        for _ in 0..384 {
            gz.write_all(&chunk).unwrap();
        }
        gz.write_all(&tar(&[member("logs/a.log", b'0', b"NEEDLE\n")])).unwrap();
        gz.finish().unwrap().flush().unwrap();

        let (peak, code, err) = peak_rss(&path);
        let flag = flag as char;
        assert!(err.len() < 2000, "{flag}: {} bytes of stderr", err.len());
        assert_eq!(code, Some(2), "{flag}: {err}");
        assert!(err.contains("1073741824"), "{flag}: {err}");
        assert!(peak < 64 * 1024, "{flag}: {peak} KiB resident");
    }
}

/// Under the limit they are read as before.
#[test]
fn long_names_and_pax_paths_still_name_the_member() {
    let long = format!("{}/a.log", "d".repeat(300));
    let mut name = long.clone().into_bytes();
    name.push(0);
    let t = tar(&[member("././@LongLink", b'L', &name), member("short", b'0', b"NEEDLE\n")]);
    let (out, err, c) = search("NEEDLE", "t.tar", &t);
    assert!(out.contains(&format!("{long}:1:NEEDLE")), "{out} / {err}");
    assert_eq!(c, Some(0));

    let t = tar(&[pax(&[("path", &long), ("mtime", "1.5")]), member("short", b'0', b"NEEDLE\n")]);
    let (out, err, c) = search("NEEDLE", "t.tar", &t);
    assert!(out.contains(&format!("{long}:1:NEEDLE")), "{out} / {err}");
    assert_eq!(c, Some(0));

    // A global header names nothing.
    let global = member("pax_global_header", b'g', pax_record("comment", "abc").as_bytes());
    let t = tar(&[global, member("short", b'0', b"NEEDLE\n")]);
    let (out, err, c) = search("NEEDLE", "t.tar", &t);
    assert!(out.contains("short:1:NEEDLE"), "{out} / {err}");
    assert_eq!(c, Some(0));
}

/// A pax `size` is the member's size, over the header's field.
#[test]
fn a_pax_size_overrides_the_headers() {
    let body = b"x\nNEEDLE\n";
    let mut m = header("logs/a.log", b'0', 0, Some(USTAR));
    m.extend(blocks(body));
    let t = tar(&[pax(&[("size", &body.len().to_string())]), m, member("logs/b.log", b'0', b"NEEDLE b\n")]);
    let (out, err, c) = search("NEEDLE", "t.tar", &t);
    assert!(out.contains("logs/a.log:2:NEEDLE"), "{out} / {err}");
    assert!(out.contains("logs/b.log:1:NEEDLE b"), "{out} / {err}");
    assert_eq!((c, err.as_str()), (Some(0), ""));
}

/// A ustar name split over the prefix and name fields.
#[test]
fn a_ustar_prefix_is_part_of_the_name() {
    let mut h = header("a.log", b'0', 7, Some(USTAR));
    h[345..353].copy_from_slice(b"var/logs");
    seal(&mut h);
    h.extend(blocks(b"NEEDLE\n"));
    let (out, err, c) = search("NEEDLE", "t.tar", &tar(&[h]));
    assert!(out.contains("var/logs/a.log:1:NEEDLE"), "{out} / {err}");
    assert_eq!(c, Some(0));
}

// ---- 5. sniffing ---------------------------------------------------------

/// `gtar --format=v7`: no magic at 257, only a header whose checksum holds.
/// Taken for a plain file, its compressed members were never opened.
#[test]
fn a_v7_tar_is_a_tar() {
    let gz = helpers::gzip_bytes(b"x\nNEEDLE inside\n");
    let mut t = header("logs/", 0, 0, None);
    t.extend(header("logs/a.log.gz", b'0', gz.len() as u64, None));
    t.extend(blocks(&gz));
    t.extend(end());
    for (name, bytes) in [("v7.tar", t.clone()), ("v7.tgz", helpers::gzip_bytes(&t))] {
        let (out, err, c) = search("NEEDLE", name, &bytes);
        assert!(out.contains("logs/a.log.gz:2:NEEDLE inside"), "{name}: {out} / {err}");
        assert_eq!((c, err.as_str()), (Some(0), ""), "{name}");
        let listed = list(name, &bytes);
        assert_eq!(listed.lines().count(), 1, "a v7 directory is a name ending in a slash: {listed}");
    }
}

/// `ustar` at byte 257 of a log, which whoever sends the first request
/// line can arrange, made the log a broken tar: a 2, and not searched.
#[test]
fn a_log_with_the_tar_magic_in_it_is_a_log() {
    let mut log = vec![b'a'; 256];
    log.extend_from_slice(b"\nustar   is not a tar\nNEEDLE in a log\n");
    log.resize(2000, b'z');
    log.push(b'\n');
    let (out, err, c) = search("NEEDLE", "access.log", &log);
    assert!(out.contains("NEEDLE in a log"), "{out} / {err}");
    assert_eq!((c, err.as_str()), (Some(0), ""));
    let (_, err, c) = search("NOPE", "access.log", &log);
    assert_eq!((c, err.as_str()), (Some(1), ""));
}

/// Zero blocks before the first header, which `tar -i` reads past.
#[test]
fn a_tar_after_leading_zero_blocks_is_a_tar() {
    let gz = helpers::gzip_bytes(b"x\nNEEDLE inside\n");
    let inner = tar(&[member("logs/a.log.gz", b'0', &gz)]);
    for zeros in [512, 10240, 512 * 1024] {
        let mut t = vec![0u8; zeros];
        t.extend_from_slice(&inner);
        for (name, bytes) in [("z.tar", t.clone()), ("z.tgz", helpers::gzip_bytes(&t))] {
            let (out, err, c) = search("NEEDLE", name, &bytes);
            assert!(out.contains("logs/a.log.gz:2:NEEDLE inside"), "{zeros} {name}: {out} / {err}");
            assert_eq!((c, err.as_str()), (Some(0), ""), "{zeros} {name}");
        }
    }
    // Zeros and then text are a file of zeros and text.
    let mut plain = vec![0u8; 2048];
    plain.extend_from_slice(b"\nNEEDLE after zeros\n");
    let (_, err, c) = search("NEEDLE", "z.bin", &plain);
    assert_eq!((c, err.as_str()), (Some(0), ""));
    let (_, err, c) = search("NEEDLE", "zeros.bin", &vec![0u8; 20480]);
    assert_eq!((c, err.as_str()), (Some(1), ""));
}

fn unreadable() -> Vec<(&'static str, Vec<u8>)> {
    let mut lzma = vec![0x5D, 0, 0, 0x80, 0];
    lzma.extend([0xFF; 8]);
    lzma.extend([0, 0x26, 0x1B, 0xCA, 0x46, 0x67, 0x5A, 0xF2, 0x77, 0xB8, 0x7D, 0x86, 0xD8, 0x41, 0xDB, 0x05]);
    let mut odc = b"070707".to_vec();
    odc.extend(std::iter::repeat_n(b'0', 53));
    odc.extend_from_slice(b"00000600000000007a.log\0NEEDLE\n");
    let mut newc = b"070701".to_vec();
    newc.extend(std::iter::repeat_n(b'0', 88));
    newc.extend_from_slice(b"0000000600000000a.log\0NEEDLE\n");
    let mut crc = newc.clone();
    crc[5] = b'2';
    let fields: [u16; 13] = [0o070707, 1, 2, 0o100644, 0, 0, 1, 0, 0, 0, 6, 0, 7];
    let mut le: Vec<u8> = fields.iter().flat_map(|f| f.to_le_bytes()).collect();
    le.extend_from_slice(b"a.log\0NEEDLE\n");
    let mut be: Vec<u8> = fields.iter().flat_map(|f| f.to_be_bytes()).collect();
    be.extend_from_slice(b"a.log\0NEEDLE\n");
    vec![
        ("lzip", b"LZIP\x01\x0c\x00\x13\x94NEEDLE\n".to_vec()),
        ("7z", b"7z\xBC\xAF\x27\x1C\x00\x04NEEDLE\n".to_vec()),
        ("rar", b"Rar!\x1a\x07\x01\x00NEEDLE\n".to_vec()),
        ("cpio", odc),
        ("cpio", newc),
        ("cpio", crc),
        ("cpio", le),
        ("cpio", be),
        ("lzop", b"\x89LZO\x00\r\n\x1a\n\x10\x40NEEDLE\n".to_vec()),
        ("lzma", lzma),
    ]
}

/// Formats trg has no reader for were searched as the bytes they are: a 1
/// about a compressed file's content.
#[test]
fn a_format_trg_cannot_read_is_a_two_not_a_search_of_its_bytes() {
    for (kind, bytes) in unreadable() {
        let (out, err, c) = search("NOPE", "x.bin", &bytes);
        assert_eq!(c, Some(2), "{kind}: {out} / {err}");
        assert!(err.contains(&format!("{kind} file, which trg cannot read: not searched")), "{kind}: {err}");
        assert!(err.contains("x.bin"), "{kind}: {err}");

        // As a member of a tar and of a zip, and under a compression.
        let t = tar(&[member("logs/x.bin", b'0', &bytes), member("logs/a.log", b'0', b"NEEDLE\n")]);
        let (out, err, c) = search("NEEDLE", "t.tar", &t);
        assert_eq!(c, Some(2), "{kind} in a tar: {out} / {err}");
        assert!(err.contains("logs/x.bin") && err.contains("cannot read"), "{kind} in a tar: {err}");
        assert!(out.contains("logs/a.log:1:NEEDLE"), "{kind}: the other members are searched: {out}");
        let z = helpers::zip_bytes(&[("x.bin", &bytes)], helpers::ZipMethod::Stored);
        let (_, err, c) = search("NOPE", "t.zip", &z);
        assert_eq!(c, Some(2), "{kind} in a zip: {err}");
        let (_, err, c) = search("NOPE", "x.bin.gz", &helpers::gzip_bytes(&bytes));
        assert_eq!(c, Some(2), "{kind} in a gzip: {err}");
        assert!(err.contains("cannot read"), "{kind} in a gzip: {err}");
    }
}

/// Their magics are mostly printable, and a log may start with one.
#[test]
fn text_that_starts_like_an_unreadable_format_is_text() {
    for text in [
        &b"LZIP is four letters\nNEEDLE\n"[..],
        b"070707 is a number\nNEEDLE\n",
        b"070701 12:00:01 GET /\nNEEDLE\n",
        b"0707020000 1111 2222 3333 4444 5555 6666 7777 8888 9999 0000 1111 2222 3333 4444 5555 6666 7777 8888 9999 0000 1111 2222\nNEEDLE\n",
        b"7z is an archiver\nNEEDLE\n",
        b"Rar! said the lion\nNEEDLE\n",
        b"q\xc7 is two bytes\nNEEDLE\n",
        b"]\nNEEDLE\n",
    ] {
        let (_, err, c) = search("NEEDLE", "a.log", text);
        assert_eq!((c, err.as_str()), (Some(0), ""), "{:?}", String::from_utf8_lossy(text));
    }
}

// ---- 6. a padded .tar.Z --------------------------------------------------

/// bsdtar pads `tar -cZf - dir > x.tar.Z` to 10 KiB. The padding decodes
/// through LZW to NULs, any number of them, after the end-of-archive blocks.
#[test]
fn zero_padding_after_a_tar_z_is_not_an_error() {
    let t = helpers::tar_bytes(&[("logs/a.log", b"x\nERR one\n")]);
    let z = helpers::z_bytes(&t);
    for pad in [1, 7, 100, 10240 - z.len()] {
        let mut bytes = z.clone();
        bytes.resize(z.len() + pad, 0);
        let (out, err, c) = search("ERR", "p.tar.Z", &bytes);
        assert!(out.contains("logs/a.log:2:ERR one"), "{pad}: {out} / {err}");
        assert_eq!(c, Some(0), "{pad}: {err}");
        assert!(!err.contains("block"), "{pad}: {err}");
        let (_, err, c) = search("NOPE", "p.tar.Z", &bytes);
        assert_eq!(c, Some(1), "{pad}: {err}");
    }
}

/// A short tail of zeros stands in for nothing but padding: a tar cut inside
/// its first end-of-archive block has still lost its end.
#[test]
fn a_tar_cut_inside_its_first_end_block_is_a_two() {
    let t = helpers::tar_bytes(&[("logs/a.log", b"x\n")]);
    let (_, err, c) = search("NOPE", "cut.tar", &t[..1024 + 100]);
    assert_eq!(c, Some(2), "{err}");
    let (_, err, c) = search("NOPE", "ok.tar", &t[..1024 + 512 + 100]);
    assert_eq!((c, err.as_str()), (Some(1), ""));
}
