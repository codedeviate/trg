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

#[test]
fn a_symlinked_file_in_a_walked_directory_is_searched() {
    let d = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let target = elsewhere.path().join("target.log");
    std::fs::write(&target, b"NEEDLE elsewhere\n").unwrap();
    std::os::unix::fs::symlink(&target, d.path().join("linked.log")).unwrap();

    let (items, errs) = trg::source::resolve(&[d.path().to_path_buf()]);
    assert!(errs.is_empty(), "got errs {errs:?}");
    let names: Vec<_> = items.iter().map(|p| p.file_name().unwrap().to_str().unwrap()).collect();
    assert_eq!(names, vec!["linked.log"], "symlinked file must be picked up, got {items:?}");
}

#[test]
fn a_broken_symlink_in_a_walked_directory_is_an_error_not_silently_skipped() {
    let d = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(d.path().join("nonexistent.log"), d.path().join("broken.log"))
        .unwrap();

    let (items, errs) = trg::source::resolve(&[d.path().to_path_buf()]);
    assert!(items.is_empty(), "a broken symlink must not be yielded as a work item: {items:?}");
    assert_eq!(errs.len(), 1, "got errs {errs:?}");
    assert!(errs[0].contains("broken.log"), "got errs {errs:?}");
}

#[test]
fn a_directory_of_ordinary_files_still_resolves_in_sorted_order() {
    let d = tempfile::tempdir().unwrap();
    for n in ["b.tgz", "a.tgz", "c.tgz"] {
        std::fs::write(d.path().join(n), b"x").unwrap();
    }
    let (items, errs) = trg::source::resolve(&[d.path().to_path_buf()]);
    assert!(errs.is_empty());
    let names: Vec<_> = items.iter().map(|p| p.file_name().unwrap().to_str().unwrap()).collect();
    assert_eq!(names, vec!["a.tgz", "b.tgz", "c.tgz"]);
}
