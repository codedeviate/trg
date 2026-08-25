mod helpers;

use std::process::Command;

fn archives(n: usize) -> (tempfile::TempDir, Vec<std::path::PathBuf>) {
    let d = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    for i in 0..n {
        // deliberately uneven sizes so completion order differs from arg order
        let pad = vec![b'x'; (n - i) * 120_000];
        let body = format!("NEEDLE archive {i}\n").into_bytes();
        let f = helpers::tgz(
            &format!("a{i}.tgz"),
            &[("logs/pad.log", &pad), ("logs/hit.log", &body)],
        );
        let dest = d.path().join(format!("a{i}.tgz"));
        std::fs::copy(&f.path, &dest).unwrap();
        paths.push(dest);
    }
    (d, paths)
}

fn hits(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|l| l.rsplit(':').next())
        .filter(|s| s.contains("NEEDLE archive"))
        .map(|s| s.trim().to_string())
        .collect()
}

#[test]
fn output_follows_argument_order_at_j1() {
    let (_d, paths) = archives(5);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_trg"));
    cmd.args(["-j", "1", "NEEDLE archive"]);
    for p in &paths {
        cmd.arg(p);
    }
    let out = cmd.output().unwrap();
    let got = hits(&String::from_utf8_lossy(&out.stdout));
    let want: Vec<String> = (0..5).map(|i| format!("NEEDLE archive {i}")).collect();
    assert_eq!(got, want);
}

#[test]
fn output_still_follows_argument_order_under_turbo() {
    let (_d, paths) = archives(5);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_trg"));
    cmd.args(["--turbo", "NEEDLE archive"]);
    for p in &paths {
        cmd.arg(p);
    }
    let out = cmd.output().unwrap();
    let got = hits(&String::from_utf8_lossy(&out.stdout));
    let want: Vec<String> = (0..5).map(|i| format!("NEEDLE archive {i}")).collect();
    assert_eq!(got, want, "ordered output is the default even in parallel");
}

#[test]
fn no_sort_still_finds_everything() {
    let (_d, paths) = archives(5);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_trg"));
    cmd.args(["--turbo", "--no-sort", "NEEDLE archive"]);
    for p in &paths {
        cmd.arg(p);
    }
    let out = cmd.output().unwrap();
    let mut got = hits(&String::from_utf8_lossy(&out.stdout));
    got.sort();
    let mut want: Vec<String> = (0..5).map(|i| format!("NEEDLE archive {i}")).collect();
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn a_match_in_one_archive_never_interleaves_with_another() {
    let (_d, paths) = archives(4);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_trg"));
    cmd.args(["--turbo", "NEEDLE"]);
    for p in &paths {
        cmd.arg(p);
    }
    let out = cmd.output().unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    // every line for a given archive must be contiguous
    let mut seen_order: Vec<String> = Vec::new();
    for line in s.lines() {
        let archive = line.split(':').next().unwrap().to_string();
        if seen_order.last() != Some(&archive) {
            assert!(
                !seen_order.contains(&archive),
                "archive {archive} reappeared after another archive's output"
            );
            seen_order.push(archive);
        }
    }
}

// ---------------------------------------------------------------------------
// Scheduler-level tests. The CLI tests above pass vacuously while `-j` is
// ignored, so these pin the properties the CLI cannot observe directly:
// that work really overlaps, that emission is incremental, and that the
// held-buffer cap is enforced.
// ---------------------------------------------------------------------------

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use trg::sched::{Done, Job};

fn jobs(n: usize) -> Vec<Job> {
    (0..n).map(|i| Job { index: i, path: std::path::PathBuf::from(format!("j{i}")) }).collect()
}

fn buf() -> termcolor::Buffer {
    termcolor::Buffer::no_color()
}

#[test]
fn sched_runs_archives_concurrently() {
    let in_flight = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);

    trg::sched::run(
        jobs(4),
        trg::sched::Config { workers: 4, sorted: true, spill_bytes: 64 * 1024 * 1024 },
        buf,
        || (),
        |_job, _c: &mut (), _b| {
            let now = in_flight.fetch_add(1, AtomicOrdering::SeqCst) + 1;
            peak.fetch_max(now, AtomicOrdering::SeqCst);
            std::thread::sleep(Duration::from_millis(150));
            in_flight.fetch_sub(1, AtomicOrdering::SeqCst);
            trg::archive::Outcome::default()
        },
        |_done: &Done| Ok(()),
    )
    .unwrap();

    assert!(
        peak.load(AtomicOrdering::SeqCst) >= 2,
        "jobs never overlapped; the pool is still sequential"
    );
}

#[test]
fn sched_emits_in_index_order_when_sorted() {
    let seen = Mutex::new(Vec::new());

    trg::sched::run(
        jobs(8),
        trg::sched::Config { workers: 4, sorted: true, spill_bytes: 64 * 1024 * 1024 },
        buf,
        || (),
        |job, _c: &mut (), _b| {
            // later jobs finish first, so completion order != argument order
            std::thread::sleep(Duration::from_millis(80 - (job.index as u64 * 8)));
            trg::archive::Outcome::default()
        },
        |done: &Done| {
            seen.lock().unwrap().push(done.index);
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(seen.into_inner().unwrap(), (0..8).collect::<Vec<_>>());
}

#[test]
fn sched_emits_ready_results_without_waiting_for_stragglers() {
    // The whole point of bounded memory: a finished job whose predecessors are
    // all emitted must be written out and freed at once, not held until every
    // worker has joined.
    let stragglers_done = Arc::new(AtomicBool::new(false));
    let leader_was_early = Arc::new(AtomicBool::new(false));

    let sd = Arc::clone(&stragglers_done);
    let le = Arc::clone(&leader_was_early);

    trg::sched::run(
        jobs(4),
        trg::sched::Config { workers: 4, sorted: true, spill_bytes: 64 * 1024 * 1024 },
        buf,
        || (),
        |job, _c: &mut (), _b| {
            if job.index > 0 {
                std::thread::sleep(Duration::from_millis(500));
                sd.store(true, AtomicOrdering::SeqCst);
            }
            trg::archive::Outcome::default()
        },
        |done: &Done| {
            if done.index == 0 && !stragglers_done.load(AtomicOrdering::SeqCst) {
                le.store(true, AtomicOrdering::SeqCst);
            }
            Ok(())
        },
    )
    .unwrap();

    assert!(
        leader_was_early.load(AtomicOrdering::SeqCst),
        "job 0 was held until every worker joined; peak memory now grows with total match volume"
    );
}

#[test]
fn sched_spills_out_of_order_rather_than_growing_unbounded() {
    let seen = Mutex::new(Vec::new());

    trg::sched::run(
        jobs(4),
        // absurdly low cap, so anything held at all must spill
        trg::sched::Config { workers: 4, sorted: true, spill_bytes: 8 },
        buf,
        || (),
        |job, _c: &mut (), b| {
            use std::io::Write;
            if job.index == 0 {
                std::thread::sleep(Duration::from_millis(500));
            }
            write!(b, "{}", "y".repeat(100)).unwrap();
            trg::archive::Outcome::default()
        },
        |done: &Done| {
            seen.lock().unwrap().push(done.index);
            Ok(())
        },
    )
    .unwrap();

    let order = seen.into_inner().unwrap();
    let mut sorted = order.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, (0..4).collect::<Vec<_>>(), "every job must be emitted exactly once");
    assert_ne!(order.first(), Some(&0), "the cap must force an out-of-order flush");
}

#[test]
fn sched_unsorted_emits_every_job_exactly_once() {
    let seen = Mutex::new(Vec::new());

    trg::sched::run(
        jobs(16),
        trg::sched::Config { workers: 4, sorted: false, spill_bytes: 64 * 1024 * 1024 },
        buf,
        || (),
        |_job, _c: &mut (), _b| trg::archive::Outcome::default(),
        |done: &Done| {
            seen.lock().unwrap().push(done.index);
            Ok(())
        },
    )
    .unwrap();

    let mut order = seen.into_inner().unwrap();
    order.sort_unstable();
    assert_eq!(order, (0..16).collect::<Vec<_>>());
}

#[test]
fn sched_carries_outcomes_back_with_their_buffers() {
    let seen = Mutex::new(Vec::new());

    trg::sched::run(
        jobs(3),
        trg::sched::Config { workers: 2, sorted: true, spill_bytes: 64 * 1024 * 1024 },
        buf,
        || (),
        |job, _c: &mut (), b| {
            use std::io::Write;
            writeln!(b, "buffer for {}", job.index).unwrap();
            let mut o = trg::archive::Outcome::default();
            o.errors.push(format!("boom {}", job.index));
            o
        },
        |done: &Done| {
            seen.lock().unwrap().push((
                String::from_utf8(done.buffer.as_slice().to_vec()).unwrap(),
                done.outcome.errors.clone(),
            ));
            Ok(())
        },
    )
    .unwrap();

    let got = seen.into_inner().unwrap();
    assert_eq!(got.len(), 3);
    for (i, (text, errs)) in got.iter().enumerate() {
        assert_eq!(text, &format!("buffer for {i}\n"));
        assert_eq!(errs, &vec![format!("boom {i}")]);
    }
}

// ---------------------------------------------------------------------------
// Fix round 1: panic safety, bounded memory, write-error propagation, and
// ordering by `Job::index` rather than by slot position.
// ---------------------------------------------------------------------------

fn cfg(workers: usize, sorted: bool) -> trg::sched::Config {
    trg::sched::Config { workers, sorted, spill_bytes: 64 * 1024 * 1024 }
}

#[test]
fn a_panicking_job_does_not_hang_the_pool() {
    // A hang is worse than a crash: during an incident it looks like a slow
    // search and gets waited on. The run happens on its own thread behind a
    // bounded `recv_timeout` so that a regression fails this test in seconds
    // instead of wedging the whole suite.
    //
    // The deliberate panic prints through the default hook; the
    // "deliberate test panic" line in this test's stderr is expected.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let seen = Mutex::new(Vec::new());
        let r = trg::sched::run(
            jobs(4),
            cfg(4, true),
            buf,
            || (),
            |job, _c: &mut (), _b| {
                assert!(job.index != 2, "deliberate test panic");
                trg::archive::Outcome::default()
            },
            |done: &Done| {
                seen.lock().unwrap().push((done.index, done.outcome.errors.clone()));
                Ok(())
            },
        );
        let _ = tx.send((r.is_ok(), seen.into_inner().unwrap()));
    });

    let (ok, got) = rx
        .recv_timeout(Duration::from_secs(15))
        .expect("the pool never finished: a panicking job hung the collector");

    assert!(ok);
    assert_eq!(
        got.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
        vec![0, 1, 2, 3],
        "a panicking job must not swallow the jobs around it, or the order"
    );
    let errs = &got[2].1;
    assert_eq!(errs.len(), 1, "the panicked archive should report one error, got {errs:?}");
    assert!(errs[0].contains("panicked"), "unhelpful error text: {errs:?}");
}

#[test]
fn a_panicking_job_is_an_error_not_silence_at_j1() {
    let seen = Mutex::new(Vec::new());
    trg::sched::run(
        jobs(3),
        cfg(1, true),
        buf,
        || (),
        |job, _c: &mut (), _b| {
            assert!(job.index != 1, "deliberate test panic");
            trg::archive::Outcome::default()
        },
        |done: &Done| {
            seen.lock().unwrap().push((done.index, done.outcome.errors.clone()));
            Ok(())
        },
    )
    .unwrap();

    let got = seen.into_inner().unwrap();
    assert_eq!(got.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![0, 1, 2]);
    assert!(got[1].1[0].contains("panicked"), "got {:?}", got[1].1);
}

#[test]
fn a_write_error_stops_the_sweep_instead_of_grinding_on() {
    // `trg PATTERN *.tgz | head -5` must stop searching, not work through a
    // month of archives writing into a closed pipe.
    let started = Arc::new(AtomicUsize::new(0));
    let s = Arc::clone(&started);

    let r = trg::sched::run(
        jobs(40),
        cfg(2, true),
        buf,
        || (),
        move |_job, _c: &mut (), _b| {
            s.fetch_add(1, AtomicOrdering::SeqCst);
            std::thread::sleep(Duration::from_millis(20));
            trg::archive::Outcome::default()
        },
        |_done: &Done| Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
    );

    assert_eq!(r.unwrap_err().kind(), std::io::ErrorKind::BrokenPipe, "the error must reach the caller");
    let n = started.load(AtomicOrdering::SeqCst);
    assert!(n < 40, "all {n} archives were searched despite a dead pipe");
}

#[test]
fn a_write_error_stops_the_sweep_at_j1_too() {
    let started = Arc::new(AtomicUsize::new(0));
    let s = Arc::clone(&started);

    let r = trg::sched::run(
        jobs(40),
        cfg(1, true),
        buf,
        || (),
        move |_job, _c: &mut (), _b| {
            s.fetch_add(1, AtomicOrdering::SeqCst);
            trg::archive::Outcome::default()
        },
        |_done: &Done| Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
    );

    assert_eq!(r.unwrap_err().kind(), std::io::ErrorKind::BrokenPipe);
    assert_eq!(started.load(AtomicOrdering::SeqCst), 1, "the run must stop at the first failed write");
}

#[test]
fn output_buffers_are_recycled_not_reallocated_per_archive() {
    // The mechanism behind peak RSS scaling with archive count: a fresh buffer
    // per archive is grown by a realloc chain to that archive's whole match
    // volume and then freed, and the allocator does not reuse the span.
    //
    // At `-j1` — the default, and the configuration the RSS regression was
    // measured in — this is exact and structural: one buffer, whatever the
    // archive count.
    let made = AtomicUsize::new(0);

    trg::sched::run(
        jobs(40),
        cfg(1, true),
        || {
            made.fetch_add(1, AtomicOrdering::SeqCst);
            termcolor::Buffer::no_color()
        },
        || (),
        |_job, _c: &mut (), _b| trg::archive::Outcome::default(),
        |_done: &Done| Ok(()),
    )
    .unwrap();

    assert_eq!(made.load(AtomicOrdering::SeqCst), 1, "-j1 must allocate exactly one buffer");
}

#[test]
fn the_pool_path_reuses_buffers_when_the_collector_keeps_up() {
    // In the pool path the buffer *count* is deliberately uncapped — holding
    // `k` results in order needs `k` buffers, so a hard cap would cap the
    // ordering window instead. What must hold is that buffers come back and get
    // reused rather than being rebuilt per archive. Jobs here take real time
    // while `emit` is instant, which is the shape of every real run: an archive
    // is tens of milliseconds of inflate, a write is microseconds.
    let made = AtomicUsize::new(0);

    trg::sched::run(
        jobs(40),
        cfg(4, true),
        || {
            made.fetch_add(1, AtomicOrdering::SeqCst);
            termcolor::Buffer::no_color()
        },
        || (),
        |_job, _c: &mut (), _b| {
            std::thread::sleep(Duration::from_millis(5));
            trg::archive::Outcome::default()
        },
        |_done: &Done| Ok(()),
    )
    .unwrap();

    let n = made.load(AtomicOrdering::SeqCst);
    // Observed 7-9 unloaded and up to 16 when the rest of this suite is
    // hammering the same cores; without pooling it is a deterministic 40. The
    // bound is set for that margin rather than tight to the typical value —
    // a tighter one flaked 2 runs in 25.
    assert!(n <= 24, "40 archives allocated {n} buffers; the pool is not being reused");
}

#[test]
fn worker_state_is_built_once_per_worker_not_once_per_job() {
    // `Searcher` is not `Sync`, so it cannot be hoisted out of the pool — but
    // building one per *archive* throws away a line buffer that has grown
    // toward the heap limit, every archive.
    let built = AtomicUsize::new(0);

    trg::sched::run(
        jobs(40),
        cfg(1, true),
        buf,
        || {
            built.fetch_add(1, AtomicOrdering::SeqCst);
        },
        |_job, _c: &mut (), _b| trg::archive::Outcome::default(),
        |_done: &Done| Ok(()),
    )
    .unwrap();

    assert_eq!(built.load(AtomicOrdering::SeqCst), 1, "-j1 must build worker state exactly once");

    let built = AtomicUsize::new(0);
    trg::sched::run(
        jobs(40),
        cfg(4, true),
        buf,
        || {
            built.fetch_add(1, AtomicOrdering::SeqCst);
        },
        |_job, _c: &mut (), _b| trg::archive::Outcome::default(),
        |_done: &Done| Ok(()),
    )
    .unwrap();

    let n = built.load(AtomicOrdering::SeqCst);
    assert_eq!(n, 4, "40 archives across 4 workers built worker state {n} times");
}

#[test]
fn emission_order_follows_job_index_not_slot_position() {
    // `main` builds index == position, so these coincide there. A future
    // caller handing us arbitrary indices must still get the order it asked
    // for, rather than silently getting argument position.
    let want = [30usize, 10, 20, 40];
    let js: Vec<Job> = want
        .iter()
        .map(|&index| Job { index, path: std::path::PathBuf::from(format!("j{index}")) })
        .collect();

    let seen = Mutex::new(Vec::new());
    trg::sched::run(
        js,
        cfg(4, true),
        buf,
        || (),
        |_job, _c: &mut (), _b| trg::archive::Outcome::default(),
        |done: &Done| {
            seen.lock().unwrap().push(done.index);
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(seen.into_inner().unwrap(), vec![10, 20, 30, 40]);
}

// ---------------------------------------------------------------------------
// Fix round 2: producer-side backpressure.
// ---------------------------------------------------------------------------

/// Runs `f` on its own thread and fails the test if it has not finished in
/// `secs`. Every backpressure test needs this: the failure mode being guarded
/// against is a deadlock, and a deadlock inside the test would wedge the suite
/// instead of reporting.
fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(v) => v,
        Err(e) => panic!("did not finish within {secs}s ({e:?}) — deadlock or missing wakeup"),
    }
}

#[test]
fn workers_are_held_back_instead_of_running_the_heap_up() {
    // The adversarial shape: far more jobs than workers, each producing output
    // well above what a single archive would, and an `emit` slow enough that
    // the collector cannot keep up. Without backpressure nothing stops a worker
    // starting the next archive, so live buffers track the *job count*: 300
    // jobs meant ~300 live megabyte buffers under a 64 MB cap, because the
    // spill flush moves bytes from `slots` into the collector's batch — which
    // decrements `held` without freeing anything.
    const JOBS: usize = 300;
    const PER_JOB: usize = 1024 * 1024;
    const SPILL: usize = 64 * 1024 * 1024;
    const WORKERS: usize = 8;

    let made = within(120, || {
        let made = Arc::new(AtomicUsize::new(0));
        let m = Arc::clone(&made);
        trg::sched::run(
            jobs(JOBS),
            trg::sched::Config { workers: WORKERS, sorted: true, spill_bytes: SPILL },
            move || {
                m.fetch_add(1, AtomicOrdering::SeqCst);
                termcolor::Buffer::no_color()
            },
            || (),
            |_job, _c: &mut (), b| {
                use std::io::Write;
                b.write_all(&vec![b'y'; PER_JOB]).unwrap();
                trg::archive::Outcome::default()
            },
            |_done: &Done| {
                std::thread::sleep(Duration::from_millis(3));
                Ok(())
            },
        )
        .unwrap();
        made.load(AtomicOrdering::SeqCst)
    });

    // Target invariant: spill_bytes + workers * max_archive_output, i.e. about
    // 64 + 8 buffers here. The generous ceiling leaves room for the overshoot
    // from workers already running when the cap is reached, while still being
    // nowhere near the 300 that no backpressure produces.
    let peak_mb = made * PER_JOB / (1024 * 1024);
    assert!(
        made <= 150,
        "{made} buffers ({peak_mb} MB) live under a {} MB cap; \
         live buffers are tracking the job count, not the window",
        SPILL / (1024 * 1024)
    );
}

#[test]
fn backpressure_does_not_deadlock_when_the_cap_is_below_one_job() {
    // The nastiest case for a producer-side wait: every single job on its own
    // exceeds the cap, so a worker is blocked from the moment the first result
    // lands. Progress depends entirely on the collector being able to drain and
    // recycle without a worker first releasing something.
    let seen = within(60, || {
        let seen: Mutex<Vec<usize>> = Mutex::new(Vec::new());
        trg::sched::run(
            jobs(24),
            trg::sched::Config { workers: 4, sorted: true, spill_bytes: 1024 },
            buf,
            || (),
            |_job, _c: &mut (), b| {
                use std::io::Write;
                b.write_all(&vec![b'y'; 64 * 1024]).unwrap();
                trg::archive::Outcome::default()
            },
            |done: &Done| {
                seen.lock().unwrap().push(done.index);
                Ok(())
            },
        )
        .unwrap();
        seen.into_inner().unwrap()
    });
    // Not an ordering assertion: a cap below a single job's output means the
    // spill valve fires by design, and abandoning the order is what it is for.
    // What must hold is that the run completes and loses nothing.
    let mut got = seen;
    assert_eq!(got.len(), 24, "every job must be emitted exactly once");
    got.sort_unstable();
    assert_eq!(got, (0..24).collect::<Vec<_>>());
}

#[test]
fn backpressure_keeps_the_order_it_is_supposed_to_keep() {
    // A cap that bounds memory must not become a cap on ordering depth — the
    // failure my first attempt produced, where results came out [3,2,1,0,...].
    let got = within(60, || {
        let seen = Mutex::new(Vec::new());
        trg::sched::run(
            jobs(64),
            trg::sched::Config { workers: 8, sorted: true, spill_bytes: 8 * 1024 * 1024 },
            buf,
            || (),
            |job, _c: &mut (), b| {
                use std::io::Write;
                // job 0 is the straggler every later job has to wait behind
                if job.index == 0 {
                    std::thread::sleep(Duration::from_millis(200));
                }
                b.write_all(&vec![b'y'; 16 * 1024]).unwrap();
                trg::archive::Outcome::default()
            },
            |done: &Done| {
                seen.lock().unwrap().push(done.index);
                Ok(())
            },
        )
        .unwrap();
        seen.into_inner().unwrap()
    });
    assert_eq!(got, (0..64).collect::<Vec<_>>(), "backpressure must not reorder output");
}

#[test]
fn a_panicking_emit_does_not_wedge_the_pool() {
    // The mirror of the worker-side hang the `Completion` guard fixed: a
    // collector that unwinds out of `emit` skips the teardown that wakes
    // workers parked on backpressure, and `thread::scope` then blocks forever
    // joining them. The buffers still sitting in the collector's batch are also
    // dropped without passing through `recycle`.
    //
    // `spill_bytes` is tiny against the per-job output so that workers are
    // reliably parked at the moment `emit` dies. The "deliberate test panic in
    // emit" line in this test's stderr is expected.
    let unwound = within(30, || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            trg::sched::run(
                jobs(200),
                trg::sched::Config { workers: 8, sorted: true, spill_bytes: 1024 },
                buf,
                || (),
                |_job, _c: &mut (), b| {
                    use std::io::Write;
                    b.write_all(&vec![b'y'; 64 * 1024]).unwrap();
                    trg::archive::Outcome::default()
                },
                |done: &Done| {
                    assert!(done.index != 3, "deliberate test panic in emit");
                    std::thread::sleep(Duration::from_millis(2));
                    Ok(())
                },
            )
        }))
        .is_err()
    });

    assert!(unwound, "emit's panic should have propagated out of run, not been swallowed");
}

/// The third deadlock of the same class, and the reason the guard is structural
/// rather than an argument about the current caller.
///
/// `Completion` is constructed only *after* a worker has claimed a job, so a
/// worker that dies before that — in `make_worker()`, or on the state lock, or
/// in `space.wait()` — used to exit without advancing `finished` and without
/// notifying. With every worker gone that way, the collector blocked on
/// `wake.wait` forever and `thread::scope` never returned.
///
/// Bounded by `within(..)` so a regression fails in seconds instead of wedging
/// the suite. The "deliberate test panic in make_worker" lines in this test's
/// stderr are expected, one per worker.
#[test]
fn a_panicking_make_worker_does_not_hang_the_pool() {
    const JOBS: usize = 6;

    let (ok, got) = within(20, || {
        let seen = Mutex::new(Vec::new());
        let r = trg::sched::run(
            jobs(JOBS),
            cfg(4, true),
            buf,
            || -> () { panic!("deliberate test panic in make_worker") },
            |_job, _c: &mut (), _b| trg::archive::Outcome::default(),
            |done: &Done| {
                seen.lock().unwrap().push((done.index, done.outcome.errors.clone()));
                Ok(())
            },
        );
        (r.is_ok(), seen.into_inner().unwrap())
    });

    assert!(ok, "a dead worker pool is a partial run, not a write error");
    assert_eq!(
        got.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
        (0..JOBS).collect::<Vec<_>>(),
        "every job must be accounted for, in order"
    );
    // The whole point of not hanging: the archives nobody read are *reported*,
    // so `main` sets `partial` and the run exits 2. Silence here would be a 1 —
    // "everything read, nothing found" — for a run that read nothing at all.
    for (i, errs) in &got {
        assert_eq!(errs.len(), 1, "job {i} should report exactly one error, got {errs:?}");
        assert!(errs[0].contains("not read"), "job {i}: unhelpful error text {errs:?}");
    }
}

/// The surviving workers must still do the whole job list: one worker whose
/// `make_worker` panics is a fault in that worker, not in the run.
#[test]
fn one_dead_worker_does_not_cost_the_run_its_archives() {
    const JOBS: usize = 40;
    let built = Arc::new(AtomicUsize::new(0));
    let b = Arc::clone(&built);

    let (ok, got) = within(20, || {
        let seen = Mutex::new(Vec::new());
        let r = trg::sched::run(
            jobs(JOBS),
            cfg(4, true),
            buf,
            move || {
                // Exactly one worker dies; the other three carry the list.
                if b.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
                    panic!("deliberate test panic in make_worker");
                }
            },
            |_job, _c: &mut (), _b| trg::archive::Outcome::default(),
            |done: &Done| {
                seen.lock().unwrap().push((done.index, done.outcome.errors.clone()));
                Ok(())
            },
        );
        (r.is_ok(), seen.into_inner().unwrap())
    });

    assert!(ok);
    assert_eq!(
        got.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
        (0..JOBS).collect::<Vec<_>>(),
        "the surviving workers must cover every job, in order"
    );
    let errs: Vec<_> = got.iter().filter(|(_, e)| !e.is_empty()).collect();
    assert!(errs.is_empty(), "nothing went unread, so nothing should be reported: {errs:?}");
    assert!(built.load(AtomicOrdering::SeqCst) >= 4, "all four workers should have started");
}
