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

/// The first decision, before any worker has started: trg contributes no load
/// yet, so the whole load average is someone else's.
#[test]
fn the_starting_gate_follows_the_load_at_startup() {
    use trg::resource::starting_gate;
    assert_eq!(starting_gate(8, 4.0, 2.0), 8, "under the limit starts wide open");
    assert_eq!(starting_gate(8, 4.0, 4.0), 8, "at the limit is not over it");
    assert_eq!(starting_gate(8, 4.0, 6.5), 1, "over the limit starts at one job");
}

#[test]
fn the_gate_drops_to_one_when_other_load_exceeds_the_limit() {
    use trg::resource::next_gate;
    // 8 of ours + 12 of theirs, limit 4.
    assert_eq!(next_gate(8, 8, 20.0, 4.0), 1);
    assert_eq!(next_gate(3, 8, 20.0, 4.0), 1);
}

/// The defect that kept the old check a one-off: trg's own workers are in the
/// load average. Compared raw, `-j8 --load-limit 4` would throttle itself
/// with Apache idle.
#[test]
fn our_own_workers_are_not_counted_as_load() {
    use trg::resource::next_gate;
    assert_eq!(next_gate(8, 8, 11.0, 4.0), 8, "8 ours + 3 theirs is under a limit of 4");
}

/// Back up one worker per check, not all at once: the load average lags, and
/// a gate that snaps open after a spike walks straight back into it.
#[test]
fn the_gate_reopens_one_worker_at_a_time() {
    use trg::resource::next_gate;
    assert_eq!(next_gate(1, 8, 1.5, 4.0), 2);
    assert_eq!(next_gate(2, 8, 2.5, 4.0), 3);
    assert_eq!(next_gate(8, 8, 8.0, 4.0), 8, "never past -j");
}

#[test]
fn the_gate_never_closes_below_one() {
    use trg::resource::{next_gate, starting_gate};
    assert_eq!(next_gate(1, 1, 99.0, 0.0), 1);
    assert_eq!(next_gate(0, 4, 0.0, 4.0), 1);
    assert_eq!(starting_gate(0, 0.0, 99.0), 1);
}

#[test]
fn a_load_average_is_readable_where_trg_ships() {
    let load = trg::resource::load_average_1m();
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        let l = load.expect("Linux and macOS both report a load average");
        assert!(l >= 0.0 && l.is_finite(), "got {l}");
    }
}

/// Without `--load-limit`, or at `-j1`, there is nothing to adapt and no
/// thread is started.
#[test]
fn no_gate_without_a_limit_or_at_one_job() {
    assert!(trg::resource::load_gate(8, None).is_none());
    assert!(trg::resource::load_gate(1, Some(0.0)).is_none());
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
