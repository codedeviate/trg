//! `sched::run_gated`: the pool behind an adaptive `--load-limit`. The gate is
//! how many workers may be inside a job at once, and it can move while the run
//! is going. These tests drive it by hand; what moves it in production is
//! `resource::load_gate`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use trg::sched::{Done, Job};

fn jobs(n: usize) -> Vec<Job> {
    (0..n).map(|i| Job { index: i, path: format!("j{i}").into() }).collect()
}

fn buf() -> termcolor::Buffer {
    termcolor::Buffer::no_color()
}

/// See the same helper in `ordering.rs`: a deadlock here must fail the test,
/// not wedge the suite.
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

/// Counts jobs in flight and remembers the most seen at once.
#[derive(Default)]
struct InFlight {
    now: AtomicUsize,
    peak: AtomicUsize,
}

impl InFlight {
    fn enter(&self) {
        let n = self.now.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(n, Ordering::SeqCst);
    }
    fn leave(&self) {
        self.now.fetch_sub(1, Ordering::SeqCst);
    }
}

#[test]
fn a_gate_of_one_runs_one_job_at_a_time_and_loses_nothing() {
    let (peak, seen) = within(30, || {
        let gate = AtomicUsize::new(1);
        let fl = InFlight::default();
        let seen = Mutex::new(Vec::new());
        trg::sched::run_gated(
            jobs(24),
            trg::sched::Config { workers: 4, sorted: true, spill_bytes: 64 << 20 },
            &gate,
            buf,
            || (),
            |_job, _c: &mut (), _b| {
                fl.enter();
                std::thread::sleep(Duration::from_millis(2));
                fl.leave();
                trg::archive::Outcome::default()
            },
            |d: &Done| {
                seen.lock().unwrap().push(d.index);
                Ok(())
            },
        )
        .unwrap();
        (fl.peak.load(Ordering::SeqCst), seen.into_inner().unwrap())
    });
    assert_eq!(peak, 1, "a gate of 1 let {peak} jobs run at once");
    assert_eq!(seen, (0..24).collect::<Vec<_>>(), "order and completeness hold under the gate");
}

/// The adaptive half: load drops, the gate opens, and workers that were held
/// back join in without the run being restarted.
#[test]
fn raising_the_gate_mid_run_lets_more_workers_in() {
    let peak = within(30, || {
        let gate = Arc::new(AtomicUsize::new(1));
        let fl = InFlight::default();
        let done = AtomicUsize::new(0);
        let g = Arc::clone(&gate);
        trg::sched::run_gated(
            jobs(40),
            trg::sched::Config { workers: 4, sorted: true, spill_bytes: 64 << 20 },
            &gate,
            buf,
            || (),
            |_job, _c: &mut (), _b| {
                fl.enter();
                if done.fetch_add(1, Ordering::SeqCst) == 4 {
                    g.store(4, Ordering::SeqCst);
                }
                std::thread::sleep(Duration::from_millis(20));
                fl.leave();
                trg::archive::Outcome::default()
            },
            |_d: &Done| Ok(()),
        )
        .unwrap();
        fl.peak.load(Ordering::SeqCst)
    });
    assert!(peak > 1, "the gate opened to 4 but jobs still ran one at a time");
}

/// The other half: load rises, the gate closes, and once the jobs already
/// running finish, no more than the new gate run at once.
#[test]
fn lowering_the_gate_mid_run_holds_workers_back() {
    let late_peak = within(30, || {
        const FIRST_WAVE: usize = 4;
        let gate = AtomicUsize::new(FIRST_WAVE);
        // The first wave blocks until all four are in flight, so the gate is
        // lowered while every worker is inside a job and none can be caught
        // between its gate check and its claim.
        let wave = (Mutex::new(0usize), Condvar::new());
        let late = InFlight::default();
        trg::sched::run_gated(
            jobs(30),
            trg::sched::Config { workers: FIRST_WAVE, sorted: false, spill_bytes: 64 << 20 },
            &gate,
            buf,
            || (),
            |job, _c: &mut (), _b| {
                if job.index < FIRST_WAVE {
                    let (m, cv) = &wave;
                    let mut n = m.lock().unwrap();
                    *n += 1;
                    if *n == FIRST_WAVE {
                        gate.store(1, Ordering::SeqCst);
                        cv.notify_all();
                    }
                    let (_n, timeout) = cv
                        .wait_timeout_while(n, Duration::from_secs(10), |n| *n < FIRST_WAVE)
                        .unwrap();
                    assert!(!timeout.timed_out(), "the first wave never filled");
                } else {
                    late.enter();
                    std::thread::sleep(Duration::from_millis(5));
                    late.leave();
                }
                trg::archive::Outcome::default()
            },
            |_d: &Done| Ok(()),
        )
        .unwrap();
        late.peak.load(Ordering::SeqCst)
    });
    assert_eq!(late_peak, 1, "the gate closed to 1 but {late_peak} jobs ran at once afterwards");
}

/// A held-back worker owns nothing, so an early stop — `trg ... | head` — must
/// not wait on it. It is the fifth way out of the pool, and it gets a test of
/// its own for the reason the module docs give: this shape has hung before.
#[test]
fn an_early_stop_does_not_wait_on_held_back_workers() {
    let r = within(10, || {
        let gate = AtomicUsize::new(1);
        trg::sched::run_gated(
            jobs(50),
            trg::sched::Config { workers: 8, sorted: true, spill_bytes: 64 << 20 },
            &gate,
            buf,
            || (),
            |_job, _c: &mut (), _b| trg::archive::Outcome::default(),
            |_d: &Done| Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
        )
    });
    assert_eq!(r.unwrap_err().kind(), std::io::ErrorKind::BrokenPipe);
}

/// A gate of 1 must not mean "worker 0 only". If the one worker allowed in is
/// the one that died, the rest have to be able to take over, or the run
/// waits forever on a worker that is gone.
#[test]
fn a_dead_worker_does_not_hold_the_only_place_at_the_gate() {
    let seen = within(10, || {
        let gate = AtomicUsize::new(1);
        let built = AtomicUsize::new(0);
        let seen = Mutex::new(Vec::new());
        trg::sched::run_gated(
            jobs(12),
            trg::sched::Config { workers: 4, sorted: true, spill_bytes: 64 << 20 },
            &gate,
            buf,
            || {
                if built.fetch_add(1, Ordering::SeqCst) == 0 {
                    panic!("injected: the first worker fails to start");
                }
            },
            |_job, _c: &mut (), _b| trg::archive::Outcome::default(),
            |d: &Done| {
                seen.lock().unwrap().push(d.index);
                Ok(())
            },
        )
        .unwrap();
        seen.into_inner().unwrap()
    });
    assert_eq!(seen, (0..12).collect::<Vec<_>>());
}

/// The gate is clamped to at least 1. A gate of 0 would admit nobody and the
/// run would never finish.
#[test]
fn a_gate_of_zero_still_makes_progress() {
    let n = within(10, || {
        let gate = AtomicUsize::new(0);
        let n = AtomicUsize::new(0);
        trg::sched::run_gated(
            jobs(6),
            trg::sched::Config { workers: 3, sorted: true, spill_bytes: 64 << 20 },
            &gate,
            buf,
            || (),
            |_job, _c: &mut (), _b| trg::archive::Outcome::default(),
            |_d: &Done| {
                n.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .unwrap();
        n.into_inner()
    });
    assert_eq!(n, 6);
}

/// Through the binary: a limit every box is over starts the run at one job,
/// and nothing about the result changes but the pace.
#[test]
fn a_limit_below_the_load_changes_the_pace_not_the_result() {
    #[path = "helpers/mod.rs"]
    mod helpers;
    let d = tempfile::tempdir().unwrap();
    let mut args: Vec<String> = ["-j", "4", "--load-limit", "-1", "NEEDLE"].map(String::from).to_vec();
    for i in 0..6 {
        let body = format!("NEEDLE {i}\n");
        let f = helpers::tgz("x.tgz", &[("logs/a.log", body.as_bytes())]);
        let dest = d.path().join(format!("{i}.tgz"));
        std::fs::copy(&f.path, &dest).unwrap();
        args.push(dest.to_str().unwrap().to_owned());
    }
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_trg")).args(&args).output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    let got: Vec<String> = out.lines().filter_map(|l| l.rsplit(':').next()).map(String::from).collect();
    let want: Vec<String> = (0..6).map(|i| format!("NEEDLE {i}")).collect();
    assert_eq!(got, want, "argument order, every archive");
    assert_eq!(o.status.code(), Some(0));
    assert!(o.stderr.is_empty(), "got: {}", String::from_utf8_lossy(&o.stderr));
}
