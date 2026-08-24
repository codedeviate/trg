mod helpers;

#[test]
fn tgz_roundtrips_through_tar() {
    let f = helpers::tgz("a.tgz", &[("logs/x.log", b"one\ntwo\n")]);
    let file = std::fs::File::open(&f.path).unwrap();
    let dec = flate2::read::MultiGzDecoder::new(file);
    let mut ar = tar::Archive::new(dec);
    let names: Vec<String> = ar
        .entries()
        .unwrap()
        .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["logs/x.log"]);
}

#[test]
fn truncated_fixture_actually_fails_to_read() {
    let big = vec![b'x'; 200_000];
    let f = helpers::tgz_truncated("t.tgz", &[("logs/a.log", &big)], 0.4);
    let file = std::fs::File::open(&f.path).unwrap();
    let dec = flate2::read::MultiGzDecoder::new(file);
    let mut ar = tar::Archive::new(dec);
    let mut saw_err = false;
    for e in ar.entries().unwrap() {
        match e {
            Ok(mut entry) => {
                let mut sink = Vec::new();
                if std::io::copy(&mut entry, &mut sink).is_err() { saw_err = true; }
            }
            Err(_) => saw_err = true,
        }
    }
    assert!(saw_err, "truncated fixture must produce a read error");
}

#[test]
fn hardlink_fixture_has_a_link_entry_with_no_content() {
    let f = helpers::tgz_with_hardlink("h.tgz");
    let file = std::fs::File::open(&f.path).unwrap();
    let dec = flate2::read::MultiGzDecoder::new(file);
    let mut ar = tar::Archive::new(dec);
    let mut kinds = Vec::new();
    for e in ar.entries().unwrap() {
        let entry = e.unwrap();
        kinds.push(entry.header().entry_type());
    }
    assert!(kinds.iter().any(|k| *k == tar::EntryType::Link),
            "expected a hardlink entry, got {kinds:?}");
}
