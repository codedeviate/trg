#![allow(dead_code)] // each test file uses a different subset

use std::io::Write;
use std::path::PathBuf;

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
