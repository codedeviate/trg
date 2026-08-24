#![allow(dead_code)] // each test file uses a different subset

use std::io::Write;
use std::path::PathBuf;

/// Owns the `TempDir`, so it must be kept alive for as long as `path` is used:
/// binding only the field (`let p = helpers::tgz(..).path;`) drops the temp dir
/// immediately and the file vanishes before you can open it.
pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub path: PathBuf,
}

fn build_tar(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (name, body) in members {
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_mtime(1_756_000_000);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_cksum();
        b.append_data(&mut h, name, &mut &body[..]).unwrap();
    }
    b.into_inner().unwrap()
}

fn write(dir: tempfile::TempDir, name: &str, bytes: &[u8]) -> Fixture {
    let path = dir.path().join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::File::create(&path).unwrap().write_all(bytes).unwrap();
    Fixture { dir, path }
}

fn gzip(raw: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(raw).unwrap();
    e.finish().unwrap()
}

/// A normal `.tgz`.
pub fn tgz(name: &str, members: &[(&str, &[u8])]) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    write(dir, name, &gzip(&build_tar(members)))
}

/// An uncompressed `.tar`, to prove sniffing handles both.
pub fn tar_plain(name: &str, members: &[(&str, &[u8])]) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    write(dir, name, &build_tar(members))
}

/// A `.tgz` cut off mid-stream, as logrotate leaves one mid-write.
pub fn tgz_truncated(name: &str, members: &[(&str, &[u8])], keep: f64) -> Fixture {
    let full = gzip(&build_tar(members));
    let n = ((full.len() as f64) * keep) as usize;
    let dir = tempfile::tempdir().unwrap();
    write(dir, name, &full[..n.max(64)])
}

/// A `.tgz` containing a hardlink entry, which carries no content of its own.
/// This is the case that silently searched nothing during the investigation.
pub fn tgz_with_hardlink(name: &str) -> Fixture {
    let body = b"NEEDLE in the real member\n";
    let mut b = tar::Builder::new(Vec::new());

    let mut h = tar::Header::new_gnu();
    h.set_size(body.len() as u64);
    h.set_mode(0o644);
    h.set_entry_type(tar::EntryType::Regular);
    h.set_cksum();
    b.append_data(&mut h, "logs/real.log", &mut &body[..]).unwrap();

    let mut l = tar::Header::new_gnu();
    l.set_size(0);
    l.set_mode(0o644);
    l.set_entry_type(tar::EntryType::Link);
    l.set_link_name("logs/real.log").unwrap();
    l.set_cksum();
    b.append_data(&mut l, "logs/linked.log", &mut std::io::empty()).unwrap();

    let raw = b.into_inner().unwrap();
    let dir = tempfile::tempdir().unwrap();
    write(dir, name, &gzip(&raw))
}

/// A member whose path exceeds tar's 100-byte name field, forcing GNU
/// `././@LongLink` or pax extension records.
pub fn tgz_with_long_name(name: &str, depth: usize) -> Fixture {
    let long = std::iter::repeat("averylongdirectorycomponent")
        .take(depth)
        .collect::<Vec<_>>()
        .join("/");
    let member = format!("{long}/access.log");
    tgz(name, &[(member.as_str(), b"NEEDLE deep\n")])
}

/// A `.tgz` whose member is itself gzipped — what logrotate actually produces.
pub fn tgz_nested_gz(name: &str, inner: &str, body: &[u8]) -> Fixture {
    let inner_gz = gzip(body);
    tgz(name, &[(inner, inner_gz.as_slice())])
}

/// Per-file page-cache residency via mmap + mincore. Linux only: this is a
/// direct port of the `resident.c` used to produce the spec's measurements.
#[cfg(target_os = "linux")]
pub fn resident_pages(path: &std::path::Path) -> (usize, usize) {
    use std::os::unix::io::AsRawFd;
    let file = std::fs::File::open(path).unwrap();
    let len = file.metadata().unwrap().len() as usize;
    assert!(len > 0, "cannot measure residency of an empty file");
    let ps = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let np = len.div_ceil(ps);
    let m = unsafe {
        libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ,
                   libc::MAP_SHARED, file.as_raw_fd(), 0)
    };
    assert_ne!(m, libc::MAP_FAILED, "mmap failed");
    let mut vec = vec![0u8; np];
    let rc = unsafe { libc::mincore(m, len, vec.as_mut_ptr() as *mut _) };
    assert_eq!(rc, 0, "mincore failed");
    let res = vec.iter().filter(|b| *b & 1 == 1).count();
    unsafe { libc::munmap(m, len) };
    (res, np)
}

/// Drop a file's pages so each measurement starts from a known state.
/// `/proc/sys/vm/drop_caches` is read-only in a container, so this is the
/// only reliable way to reset.
#[cfg(target_os = "linux")]
pub fn evict(path: &std::path::Path) {
    use std::os::unix::io::AsRawFd;
    let file = std::fs::File::open(path).unwrap();
    unsafe {
        libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
    }
}

/// ~8 MB of mostly-unique log text, so residency figures are unambiguous.
pub fn bulky_log() -> Vec<u8> {
    (0..300_000u32)
        .flat_map(|i| format!("2026-08-24T00:00:00Z req {i} GET /path/{i}?q={i}\n").into_bytes())
        .collect()
}
