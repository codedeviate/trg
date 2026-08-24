#[test]
fn files_are_taken_in_argument_order() {
    let d = tempfile::tempdir().unwrap();
    for n in ["c.tgz", "a.tgz", "b.tgz"] {
        std::fs::write(d.path().join(n), b"x").unwrap();
    }
    let given: Vec<_> = ["c.tgz", "a.tgz", "b.tgz"].iter().map(|n| d.path().join(n)).collect();
    let (items, errs) = trg::source::resolve(&given);
    let names: Vec<_> = items.iter().map(|p| p.file_name().unwrap().to_str().unwrap()).collect();
    assert_eq!(names, vec!["c.tgz", "a.tgz", "b.tgz"]);
    assert!(errs.is_empty());
}

#[test]
fn directories_are_walked_and_sorted_for_determinism() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("sub")).unwrap();
    std::fs::write(d.path().join("b.tgz"), b"x").unwrap();
    std::fs::write(d.path().join("a.tgz"), b"x").unwrap();
    std::fs::write(d.path().join("sub/c.tgz"), b"x").unwrap();
    let (items, errs) = trg::source::resolve(&[d.path().to_path_buf()]);
    assert_eq!(items.len(), 3, "got {items:?}");
    assert!(errs.is_empty());
    let names: Vec<_> = items.iter().map(|p| p.file_name().unwrap().to_str().unwrap()).collect();
    assert_eq!(names, vec!["a.tgz", "b.tgz", "c.tgz"]);
}

#[test]
fn a_missing_path_is_an_error_not_a_panic() {
    let (items, errs) = trg::source::resolve(&[std::path::PathBuf::from("/no/such/file.tgz")]);
    assert!(items.is_empty());
    assert_eq!(errs.len(), 1);
    assert!(errs[0].contains("/no/such/file.tgz"));
}

#[test]
fn no_globs_means_none_not_an_empty_set() {
    assert!(trg::source::build_globs(&[]).unwrap().is_none());
    assert!(trg::source::build_globs(&["*.log".into()]).unwrap().is_some());
}

#[test]
fn an_invalid_glob_is_an_error() {
    assert!(trg::source::build_globs(&["[".into()]).is_err());
}
