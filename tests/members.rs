mod helpers;

use std::io::Read;

fn collect(path: &std::path::Path) -> (Vec<String>, Vec<String>) {
    let rdr = trg::archive::open_decoded(path).unwrap();
    let mut names = Vec::new();
    let out = trg::archive::for_each_member(rdr, None, |name, r| {
        let mut s = String::new();
        r.read_to_string(&mut s).ok();
        names.push(format!("{name}={}", s.trim_end()));
        Ok(())
    });
    (names, out.errors)
}

#[test]
fn yields_regular_members_only() {
    let f = helpers::tgz("a.tgz", &[
        ("logs/a.log", b"alpha\n"),
        ("logs/b.log", b"beta\n"),
    ]);
    let (names, errs) = collect(&f.path);
    assert_eq!(names, vec!["logs/a.log=alpha", "logs/b.log=beta"]);
    assert!(errs.is_empty());
}

#[test]
fn skips_hardlink_members_entirely() {
    let f = helpers::tgz_with_hardlink("h.tgz");
    let (names, errs) = collect(&f.path);
    assert_eq!(names.len(), 1, "hardlink must not be yielded: {names:?}");
    assert!(names[0].starts_with("logs/real.log="));
    assert!(errs.is_empty(), "a skipped hardlink is not an error");
}

#[test]
fn long_member_names_survive_intact() {
    let f = helpers::tgz_with_long_name("l.tgz", 6);
    let (names, _) = collect(&f.path);
    assert_eq!(names.len(), 1);
    assert!(names[0].contains("averylongdirectorycomponent"));
    assert!(!names[0].contains("@LongLink"), "pseudo-member leaked into output");
    assert!(!names[0].contains("PaxHeader"), "pseudo-member leaked into output");
}

#[test]
fn nested_gz_member_is_decompressed_one_level() {
    let f = helpers::tgz_nested_gz("n.tgz", "logs/access.log.1.gz", b"NEEDLE nested\n");
    let (names, errs) = collect(&f.path);
    assert_eq!(names.len(), 1);
    assert!(names[0].contains("NEEDLE nested"), "got {names:?}");
    assert!(errs.is_empty());
}

#[test]
fn uncompressed_tar_also_works() {
    let f = helpers::tar_plain("a.tar", &[("logs/a.log", b"alpha\n")]);
    let (names, _) = collect(&f.path);
    assert_eq!(names, vec!["logs/a.log=alpha"]);
}

#[test]
fn plain_file_is_one_anonymous_member() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("access.log");
    std::fs::write(&p, b"plain line\n").unwrap();
    let rdr = trg::archive::open_decoded(&p).unwrap();
    let mut got = Vec::new();
    let out = trg::archive::for_each_member(rdr, None, |name, r| {
        let mut s = String::new();
        r.read_to_string(&mut s).ok();
        got.push((name.to_string(), s));
        Ok(())
    });
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].0, "", "a plain file has no member name");
    assert!(out.errors.is_empty());
}

#[test]
fn truncated_archive_records_an_error_but_keeps_earlier_members() {
    let big = vec![b'x'; 300_000];
    let f = helpers::tgz_truncated("t.tgz", &[
        ("logs/first.log", b"NEEDLE early\n"),
        ("logs/second.log", &big),
    ], 0.5);
    let rdr = trg::archive::open_decoded(&f.path).unwrap();
    let mut names = Vec::new();
    let out = trg::archive::for_each_member(rdr, None, |name, r| {
        let mut sink = Vec::new();
        std::io::copy(&mut r.take(64), &mut sink)?;
        names.push(name.to_string());
        Ok(())
    });
    assert!(names.iter().any(|n| n == "logs/first.log"),
            "must keep members read before the truncation: {names:?}");
    assert!(!out.errors.is_empty(), "truncation must be recorded as an error");
}

#[test]
fn member_globs_filter_which_members_are_searched() {
    let f = helpers::tgz("g.tgz", &[
        ("logs/a.access.log", b"alpha\n"),
        ("logs/a.error.log", b"beta\n"),
    ]);
    let mut b = globset::GlobSetBuilder::new();
    b.add(globset::Glob::new("*.access.log").unwrap());
    let set = b.build().unwrap();

    let rdr = trg::archive::open_decoded(&f.path).unwrap();
    let mut names = Vec::new();
    trg::archive::for_each_member(rdr, Some(&set), |name, _| {
        names.push(name.to_string());
        Ok(())
    });
    assert_eq!(names, vec!["logs/a.access.log"]);
}
