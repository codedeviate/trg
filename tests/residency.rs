mod helpers;

/// The spec's page-cache result, turned into a regression test.
/// Linux only: macOS has no posix_fadvise, and F_NOCACHE prevents caching
/// rather than evicting, so there is nothing equivalent to assert.
#[cfg(target_os = "linux")]
#[test]
fn searching_an_archive_leaves_no_page_cache_behind() {
    let body = helpers::bulky_log();
    let f = helpers::tgz("r.tgz", &[("logs/big.log", &body)]);

    // Control: a plain read must leave pages resident. If this fails, the
    // measurement itself is broken and the real assertion below is worthless.
    helpers::evict(&f.path);
    let _ = std::fs::read(&f.path).unwrap();
    let (res, np) = helpers::resident_pages(&f.path);
    println!("control (plain read):   {res}/{np} pages resident");
    assert!(res * 2 > np,
            "control failed: a plain read left only {res}/{np} pages resident");

    helpers::evict(&f.path);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_trg"))
        .args(["req 42 GET", f.path.to_str().unwrap()])
        .output().unwrap();
    assert_eq!(out.status.code(), Some(0), "expected a match; stderr: {}",
               String::from_utf8_lossy(&out.stderr));
    let (res, np) = helpers::resident_pages(&f.path);
    println!("after trg (drop-cache): {res}/{np} pages resident");
    assert!(res * 20 < np, "trg left {res}/{np} pages resident; expected near zero");
}

/// The opt-out must actually opt out, otherwise `--no-drop-cache` is a lie.
#[cfg(target_os = "linux")]
#[test]
fn no_drop_cache_keeps_the_pages() {
    let body = helpers::bulky_log();
    let f = helpers::tgz("r2.tgz", &[("logs/big.log", &body)]);

    helpers::evict(&f.path);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_trg"))
        .args(["--no-drop-cache", "req 42 GET", f.path.to_str().unwrap()])
        .output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let (res, np) = helpers::resident_pages(&f.path);
    println!("after trg --no-drop-cache: {res}/{np} pages resident");
    assert!(res * 2 > np,
            "--no-drop-cache left only {res}/{np} pages resident; the flag is not being honoured");
}

#[test]
fn clamp_jobs_respects_the_load_limit() {
    assert_eq!(trg::resource::clamp_jobs(8, None), 8);
    assert_eq!(trg::resource::clamp_jobs(8, Some(1e9)), 8, "under limit is untouched");
    // On Linux a limit of 0.0 is always exceeded; on other platforms
    // load_average_1m() returns None and the count is left alone.
    let clamped = trg::resource::clamp_jobs(8, Some(0.0));
    if cfg!(target_os = "linux") {
        assert_eq!(clamped, 1, "over limit must clamp to 1");
    } else {
        assert_eq!(clamped, 8, "no loadavg source means no clamping");
    }
}

#[test]
fn setting_priority_is_safe_and_idempotent() {
    trg::resource::set_priority(Some(5));
    trg::resource::set_priority(Some(5));
    trg::resource::set_priority(None);
    trg::resource::set_io_idle();
}

/// A live Apache logfile is held open for append by another process; dropping
/// its pages would evict cache that process is actively using. Compressed and
/// tar inputs are cold and ours to drop; plain files must be left alone.
#[cfg(target_os = "linux")]
#[test]
fn a_plain_logfile_is_never_dropped_from_the_cache() {
    let body = helpers::bulky_log();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("access.log");
    std::fs::write(&path, &body).unwrap();

    helpers::evict(&path);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_trg"))
        .args(["req 42 GET", path.to_str().unwrap()])
        .output().unwrap();
    assert_eq!(out.status.code(), Some(0), "expected a match; stderr: {}",
               String::from_utf8_lossy(&out.stderr));
    let (res, np) = helpers::resident_pages(&path);
    println!("plain logfile after trg: {res}/{np} pages resident");
    assert!(res * 2 > np,
            "a plain file lost its page cache ({res}/{np} resident); \
             only archives may be dropped");
}
