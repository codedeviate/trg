//! Bounded worker pool. Concurrency lives at the archive boundary and nowhere
//! else, because gzip has no index: member N requires inflating 1..N-1.
//!
//! # Ordering
//!
//! Output stays in argument order by default: these archives are days, and
//! argument order is chronological order. That is achieved by holding a
//! completed archive's buffer until every earlier archive has been emitted —
//! **not** by joining all workers first. A finished job whose predecessors are
//! already out is written and freed immediately, so nothing waits on the
//! slowest archive to be handed to `emit`.
//!
//! # What actually bounds memory
//!
//! Three separate things, and it is worth being precise about which does what,
//! because `spill_bytes` alone does far less than its name suggests:
//!
//! 1. **Buffer recycling.** Output buffers are owned by this module and pooled.
//!    A worker takes one, fills it, and the collector `clear()`s it after
//!    emitting and returns it to the pool. Allocating a fresh buffer per
//!    archive instead makes peak RSS climb with the *archive count*: each one
//!    is grown by a realloc chain to that archive's whole match volume and then
//!    freed, and the allocator does not hand the same span back for the next
//!    one. Measured at `-j1` on six archives of 120k matching lines: 84 MB
//!    per-job vs. 22 MB pooled.
//!
//!    At `workers == 1` exactly one buffer exists for the whole run, which is
//!    the case that matters most since `-j1` is the default. In the pool path
//!    the *count* of live buffers is not capped, and deliberately so: holding
//!    `k` results in order requires `k` buffers, so a hard cap would be a cap
//!    on the ordering window. It is not needed either — an unwritten
//!    `termcolor::Buffer` is an empty `Vec` that has allocated nothing, and the
//!    ones that did allocate are bounded in *bytes* by `spill_bytes` below.
//!    What the pool guarantees is that a buffer's allocation is reused rather
//!    than rebuilt, which is what the realloc chain cost.
//! 2. **Per-worker state.** `make_worker` is called once per worker thread, not
//!    once per job, for the same reason — see [`run`].
//! 3. **`spill_bytes`.** This bounds only the buffers *parked* in `slots`
//!    waiting for their turn in the order. It does **not** bound peak RSS: the
//!    `workers` buffers currently being filled, and the batch the collector has
//!    drained but not yet emitted, are outside the count. Treat it as "how far
//!    out of order are we willing to hold results", not as a memory ceiling. A
//!    real run peaked at 286 MB under a 64 MB cap.
//!
//! When parked buffers do exceed `spill_bytes`, they are flushed out of order
//! with a note on stderr. Holding results in order is worth a lot, but not
//! unboundedly much when a single slow archive is pinning the rest.
//!
//! # Design
//!
//! One `Mutex` guards the slot vector, the buffer pool, and a little
//! bookkeeping; one `Condvar` wakes the collector. Workers only ever *deposit*:
//! pull the next index with a `fetch_add`, do the work outside the lock, then
//! take the lock just long enough to move the finished [`Done`] into its slot
//! and notify. Nothing a worker does can block on another worker.
//!
//! The calling thread is the collector. It owns `head` and `emitted`, neither
//! of which is shared, and it calls `emit` *outside* the lock so a slow stdout
//! — a pipe into `head(1)`, say — never stalls a worker mid-archive.
//!
//! Emission order is correct because a slot is written exactly once (position
//! `p` is handed to exactly one worker by the atomic counter) and read exactly
//! once (only the collector takes, and only from a slot it then marks emitted).
//! In sorted mode the collector walks `order`, so it emits the job with the
//! next-lowest [`Job::index`] only once every lower index has gone out. The one
//! deliberate exception is the spill above.
//!
//! # Panics and early stop
//!
//! A panicking job must not be able to hang the process — during an incident a
//! hang looks like a slow search and gets waited on, which is strictly worse
//! than a crash. Two independent mechanisms guarantee it cannot:
//! [`catch_unwind`](std::panic::catch_unwind) around the call to `work`, which
//! turns a panic into an ordinary error on that archive, and a `Completion`
//! drop guard that advances the finished count and wakes the collector even if
//! a worker unwinds anyway.
//!
//! `emit` returns `io::Result`, and the first error stops the run: no further
//! job is started and [`run`] returns the error. That is what makes
//! `trg ... | head -5` stop searching instead of grinding through a whole
//! month of archives writing into a closed pipe.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

pub struct Job {
    pub index: usize,
    pub path: std::path::PathBuf,
}

pub struct Done {
    pub index: usize,
    pub buffer: termcolor::Buffer,
    pub outcome: crate::archive::Outcome,
}

/// How the pool runs, as opposed to what it runs.
pub struct Config {
    /// Archives searched concurrently. `1` takes a dedicated sequential path.
    pub workers: usize,
    /// Emit in [`Job::index`] order rather than completion order.
    pub sorted: bool,
    /// How many bytes of *parked* output may wait for their turn before the
    /// order is abandoned. Not a memory ceiling — see the module docs.
    pub spill_bytes: usize,
}

/// Everything the workers and the collector share. The lock is held only for
/// the moves in and out of `slots` and `pool`.
struct Shared {
    /// One slot per job position. `Some` means finished and not yet emitted.
    slots: Vec<Option<Done>>,
    /// Positions in completion order, for `--no-sort`.
    ready: VecDeque<usize>,
    /// Output buffers not currently in use, kept so their allocations are
    /// reused instead of being rebuilt per archive.
    pool: Vec<termcolor::Buffer>,
    /// Bytes currently parked in `slots`, for the spill check.
    held: usize,
    /// Jobs finished so far, so the collector knows when to stop waiting.
    finished: usize,
}

/// Advances the finished count and wakes the collector on drop, so that a
/// worker which unwinds still counts as having completed its job.
///
/// Without this, a panicking worker would leave `finished` short of `n`
/// forever: the collector would wait on a notification that never comes, and
/// `thread::scope` would never return. Belt and braces alongside the
/// `catch_unwind` in [`run_job`], which already stops a panic in `work` itself;
/// this covers everything else on the path.
struct Completion<'a> {
    state: &'a Mutex<Shared>,
    wake: &'a Condvar,
    armed: bool,
}

impl Drop for Completion<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Never `unwrap` here: panicking inside a `Drop` that is itself running
        // during an unwind aborts the process. A poisoned lock is handled by
        // waking the collector anyway, which will then fail on its own lock and
        // propagate a panic — noisy, but not a hang.
        if let Ok(mut st) = self.state.lock() {
            st.finished += 1;
        }
        self.wake.notify_all();
    }
}

/// Call `work`, converting a panic into an error on that archive.
///
/// The buffer is cleared on panic: a job that died mid-write may have left a
/// partial line behind, and half a log line in the middle of ordered output is
/// worse than none.
///
/// This is a no-op under `panic = "abort"`, which the release profile sets. It
/// is still worth having: an abort is a crash, and a crash is a diagnosable
/// outcome. The failure mode being prevented is the *hang*, and the
/// `Completion` guard prevents that on both profiles.
fn run_job<C, F>(
    job: &Job,
    ctx: &mut C,
    buf: &mut termcolor::Buffer,
    work: &F,
) -> crate::archive::Outcome
where
    F: Fn(&Job, &mut C, &mut termcolor::Buffer) -> crate::archive::Outcome,
{
    // `AssertUnwindSafe` is honest here rather than a silencer: everything
    // reachable is either this job's own scratch state, which is discarded on
    // the panic path, or the shared slot vector, which this job has not touched
    // yet — it deposits only after `work` returns.
    let called = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(job, ctx, buf)));
    match called {
        Ok(o) => o,
        Err(_) => {
            buf.clear();
            let mut o = crate::archive::Outcome::default();
            o.errors.push(format!(
                "{}: panicked while searching; skipped",
                job.path.display()
            ));
            o
        }
    }
}

/// Run `work` over `jobs`, emitting each result through `emit`.
///
/// `make_worker` is called **once per worker thread**, not once per job. That
/// is the correct granularity for anything a job needs but cannot share:
/// `grep_searcher::Searcher` is not `Sync`, so it cannot be hoisted out of the
/// pool entirely, but building one per *archive* is a real cost — its internal
/// line buffer grows toward the configured heap limit and is thrown away every
/// archive. Measured on 60 small archives: 288 MB per-job vs. 30 MB per-worker.
/// At `workers == 1` this means exactly one is ever built.
///
/// `emit` borrows its [`Done`] rather than taking it, so the buffer can be
/// recycled afterwards. The first `Err` it returns stops the run: no further
/// job is started, and that error is returned.
pub fn run<C, F, E>(
    jobs: Vec<Job>,
    cfg: Config,
    make_buffer: impl Fn() -> termcolor::Buffer + Send + Sync,
    make_worker: impl Fn() -> C + Send + Sync,
    work: F,
    mut emit: E,
) -> std::io::Result<()>
where
    F: Fn(&Job, &mut C, &mut termcolor::Buffer) -> crate::archive::Outcome + Send + Sync,
    E: FnMut(&Done) -> std::io::Result<()>,
{
    let Config { workers, sorted, spill_bytes } = cfg;
    let n = jobs.len();
    if n == 0 {
        return Ok(());
    }

    // The sequential path holds nothing and needs no lock, which is already the
    // bounded-memory ideal. Kept separate rather than folded into the pool:
    // `-j1` is the production default and deserves no threads at all. One
    // worker context and one buffer for the whole run.
    if workers <= 1 {
        let mut ctx = make_worker();
        let mut buf = make_buffer();
        for job in &jobs {
            buf.clear();
            let outcome = run_job(job, &mut ctx, &mut buf, &work);
            let done = Done { index: job.index, buffer: buf, outcome };
            let r = emit(&done);
            buf = done.buffer;
            r?;
            // One pathological archive should not leave its whole match volume
            // reserved for the rest of the run.
            if buf.as_slice().len() > spill_bytes {
                buf = make_buffer();
            }
        }
        return Ok(());
    }

    // Emission order. Sorting by `Job::index` rather than by slot position
    // means a caller that hands us indices which are not `0..n` still gets the
    // order it asked for, and that `Done::index` is the one ordering key
    // everywhere — including the spill loop. Stable, so equal indices keep
    // argument order. For `main`, where index == position, this is the identity.
    let mut order: Vec<usize> = (0..n).collect();
    if sorted {
        order.sort_by_key(|&p| jobs[p].index);
    }

    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let state = Mutex::new(Shared {
        slots: (0..n).map(|_| None).collect(),
        ready: VecDeque::new(),
        pool: Vec::new(),
        held: 0,
        finished: 0,
    });
    let wake = Condvar::new();

    let mut write_err: Option<std::io::Error> = None;

    let live = workers.min(n);

    std::thread::scope(|scope| {
        for _ in 0..live {
            let (next, stop, state, wake) = (&next, &stop, &state, &wake);
            let (jobs, work, make_buffer, make_worker) =
                (&jobs, &work, &make_buffer, &make_worker);
            scope.spawn(move || {
                let mut ctx = make_worker();
                loop {
                    // Checked before claiming work, so a write failure or a
                    // closed pipe stops the sweep at the next archive boundary
                    // instead of at the end of the month.
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let p = next.fetch_add(1, Ordering::Relaxed);
                    if p >= n {
                        return;
                    }

                    let mut guard = Completion { state, wake, armed: true };

                    let mut buf = {
                        let mut st = state.lock().unwrap();
                        st.pool.pop()
                    }
                    .unwrap_or_else(make_buffer);

                    let outcome = run_job(&jobs[p], &mut ctx, &mut buf, work);
                    let bytes = buf.as_slice().len();
                    let done = Done { index: jobs[p].index, buffer: buf, outcome };

                    {
                        let mut st = state.lock().unwrap();
                        st.slots[p] = Some(done);
                        st.ready.push_back(p);
                        st.held += bytes;
                        st.finished += 1;
                        guard.armed = false;
                    }
                    wake.notify_all();
                }
            });
        }

        // The collector. Runs on the calling thread so `emit` needs to be
        // neither `Send` nor `Sync`, and so ordering decisions live in exactly
        // one place. `head` walks `order`, not raw positions.
        let mut head = 0usize;
        let mut emitted = vec![false; n];
        let mut emitted_count = 0usize;
        let mut batch: Vec<Done> = Vec::new();

        'collect: while emitted_count < n {
            {
                let mut st = state.lock().unwrap();
                loop {
                    if sorted {
                        // Drain the in-order prefix: everything from `head`
                        // onwards, in index order, that has landed.
                        while head < n {
                            if emitted[head] {
                                head += 1;
                                continue;
                            }
                            match st.slots[order[head]].take() {
                                Some(d) => {
                                    st.held -= d.buffer.as_slice().len();
                                    emitted[head] = true;
                                    head += 1;
                                    batch.push(d);
                                }
                                None => break,
                            }
                        }
                        // Checked every pass, not only when blocked, so a fast
                        // producer cannot outrun the cap while we are emitting.
                        if st.held > spill_bytes {
                            eprintln!(
                                "trg: held output exceeded {spill_bytes} bytes; \
                                 flushing out of order"
                            );
                            flush_parked(&mut st, &order, &mut emitted, head, &mut batch);
                        }
                    } else {
                        while let Some(p) = st.ready.pop_front() {
                            if let Some(d) = st.slots[p].take() {
                                st.held -= d.buffer.as_slice().len();
                                batch.push(d);
                            }
                        }
                    }

                    if !batch.is_empty() {
                        break;
                    }
                    // Not a spurious-wakeup hazard: `finished == n` implies
                    // every unemitted slot is `Some`, so the drain above would
                    // have filled `batch`. Reaching here means work is still
                    // outstanding and some worker will notify — the `Completion`
                    // guard makes that true even for a worker that unwinds.
                    debug_assert!(st.finished < n, "collector stalled with all work finished");
                    if st.finished >= n {
                        break;
                    }
                    st = wake.wait(st).unwrap();
                }
            }

            // Unreachable per the invariant above; a `break` rather than a spin
            // so a bug degrades into short output instead of a hung process.
            if batch.is_empty() {
                break;
            }

            // Emit outside the lock: stdout may block, workers must not.
            for d in batch.drain(..) {
                emitted_count += 1;
                let r = emit(&d);
                recycle(&state, d.buffer, spill_bytes, live);
                if let Err(e) = r {
                    stop.store(true, Ordering::Relaxed);
                    write_err = Some(e);
                    break 'collect;
                }
            }
        }

        // Anything still parked is dropped on the way out of the scope; only
        // reachable on the early-stop path, where it is output nobody wants.
        stop.store(true, Ordering::Relaxed);
    });

    match write_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Return a buffer to the pool so its allocation is reused, unless it grew past
/// the spill cap — one pathological archive should not reserve its whole match
/// volume for the rest of the run. `termcolor::Buffer` exposes no `capacity`,
/// so the length it reached stands in for it.
///
/// The pool is capped at the worker count: a buffer beyond that is one the
/// workers demonstrably did not need, and keeping it would hold its allocation
/// for the rest of the run.
fn recycle(state: &Mutex<Shared>, mut buffer: termcolor::Buffer, spill_bytes: usize, cap: usize) {
    if buffer.as_slice().len() > spill_bytes {
        return;
    }
    buffer.clear();
    if let Ok(mut st) = state.lock()
        && st.pool.len() < cap
    {
        st.pool.push(buffer);
    }
}

/// Emit everything that has landed but is out of turn, in index order. The
/// escape valve behind both the byte cap and buffer starvation.
fn flush_parked(
    st: &mut Shared,
    order: &[usize],
    emitted: &mut [bool],
    head: usize,
    batch: &mut Vec<Done>,
) {
    for (h, flag) in emitted.iter_mut().enumerate().skip(head) {
        if let Some(d) = st.slots[order[h]].take() {
            st.held -= d.buffer.as_slice().len();
            *flag = true;
            batch.push(d);
        }
    }
}
