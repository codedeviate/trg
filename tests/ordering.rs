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
        4,
        true,
        64 * 1024 * 1024,
        buf,
        |_job, _b| {
            let now = in_flight.fetch_add(1, AtomicOrdering::SeqCst) + 1;
            peak.fetch_max(now, AtomicOrdering::SeqCst);
            std::thread::sleep(Duration::from_millis(150));
            in_flight.fetch_sub(1, AtomicOrdering::SeqCst);
            trg::archive::Outcome::default()
        },
        |_done: Done| {},
    );

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
        4,
        true,
        64 * 1024 * 1024,
        buf,
        |job, _b| {
            // later jobs finish first, so completion order != argument order
            std::thread::sleep(Duration::from_millis(80 - (job.index as u64 * 8)));
            trg::archive::Outcome::default()
        },
        |done: Done| seen.lock().unwrap().push(done.index),
    );

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
        4,
        true,
        64 * 1024 * 1024,
        buf,
        |job, _b| {
            if job.index > 0 {
                std::thread::sleep(Duration::from_millis(500));
                sd.store(true, AtomicOrdering::SeqCst);
            }
            trg::archive::Outcome::default()
        },
        |done: Done| {
            if done.index == 0 && !stragglers_done.load(AtomicOrdering::SeqCst) {
                le.store(true, AtomicOrdering::SeqCst);
            }
        },
    );

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
        4,
        true,
        8, // absurdly low cap, so anything held at all must spill
        buf,
        |job, b| {
            use std::io::Write;
            if job.index == 0 {
                std::thread::sleep(Duration::from_millis(500));
            }
            write!(b, "{}", "y".repeat(100)).unwrap();
            trg::archive::Outcome::default()
        },
        |done: Done| seen.lock().unwrap().push(done.index),
    );

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
        4,
        false,
        64 * 1024 * 1024,
        buf,
        |_job, _b| trg::archive::Outcome::default(),
        |done: Done| seen.lock().unwrap().push(done.index),
    );

    let mut order = seen.into_inner().unwrap();
    order.sort_unstable();
    assert_eq!(order, (0..16).collect::<Vec<_>>());
}

#[test]
fn sched_carries_outcomes_back_with_their_buffers() {
    let seen = Mutex::new(Vec::new());

    trg::sched::run(
        jobs(3),
        2,
        true,
        64 * 1024 * 1024,
        buf,
        |job, b| {
            use std::io::Write;
            writeln!(b, "buffer for {}", job.index).unwrap();
            let mut o = trg::archive::Outcome::default();
            o.errors.push(format!("boom {}", job.index));
            o
        },
        |done: Done| {
            seen.lock().unwrap().push((
                String::from_utf8(done.buffer.as_slice().to_vec()).unwrap(),
                done.outcome.errors,
            ));
        },
    );

    let got = seen.into_inner().unwrap();
    assert_eq!(got.len(), 3);
    for (i, (text, errs)) in got.iter().enumerate() {
        assert_eq!(text, &format!("buffer for {i}\n"));
        assert_eq!(errs, &vec![format!("boom {i}")]);
    }
}
