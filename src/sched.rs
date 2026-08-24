//! Bounded worker pool. Concurrency lives at the archive boundary and nowhere
//! else, because gzip has no index: member N requires inflating 1..N-1.
//!
//! # Ordering and memory
//!
//! Output stays in argument order by default: these archives are days, and
//! argument order is chronological order. That is achieved by holding a
//! completed archive's buffer until every earlier archive has been emitted —
//! **not** by joining all workers first. A finished job whose predecessors are
//! already out is written and freed immediately, so peak memory tracks the
//! in-flight window rather than the total match volume. Waiting for the join
//! would defeat the whole point of a tool built for bounded memory, and would
//! make the spill cap below decorative.
//!
//! # Design
//!
//! One `Mutex` guards a slot vector plus a small amount of bookkeeping, and one
//! `Condvar` wakes the collector. Workers only ever *deposit*: pull the next
//! index with a `fetch_add`, do the work outside the lock, then take the lock
//! just long enough to move the finished [`Done`] into its slot and notify.
//! Nothing a worker does can block on another worker.
//!
//! The calling thread is the collector. It owns `head` (the lowest index not
//! yet emitted) and `emitted`, neither of which is shared, and it calls `emit`
//! *outside* the lock so a slow stdout — a pipe into `head(1)`, say — never
//! stalls a worker mid-archive.
//!
//! Emission order is correct because a slot is written exactly once (index `i`
//! is handed to exactly one worker by the atomic counter) and read exactly once
//! (only the collector takes, and only from a slot it then marks emitted). The
//! collector emits `head` only when `slots[head]` is `Some`, so in sorted mode
//! index `i` can never precede index `j < i` unless `j` was already emitted.
//! The one deliberate exception is the spill below.
//!
//! If held-but-not-yet-emittable buffers exceed `spill_bytes`, they are flushed
//! out of order with a note on stderr. Bounded memory beats perfect ordering
//! when a single slow archive would otherwise pin gigabytes of matches.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
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

/// Everything the workers and the collector share. Deliberately small: the
/// lock is held only for the moves in and out of `slots`.
struct Shared {
    /// One slot per job position. `Some` means finished and not yet emitted.
    slots: Vec<Option<Done>>,
    /// Positions in completion order, for `--no-sort`.
    ready: VecDeque<usize>,
    /// Bytes currently parked in `slots`, for the spill check.
    held: usize,
    /// Jobs finished so far, so the collector knows when to stop waiting.
    finished: usize,
}

/// Run `work` over `jobs` with at most `workers` in flight, emitting results
/// through `emit`. When `sorted`, results are emitted in job order.
pub fn run<F, E>(
    jobs: Vec<Job>,
    workers: usize,
    sorted: bool,
    spill_bytes: usize,
    make_buffer: impl Fn() -> termcolor::Buffer + Send + Sync,
    work: F,
    mut emit: E,
) where
    F: Fn(&Job, &mut termcolor::Buffer) -> crate::archive::Outcome + Send + Sync,
    E: FnMut(Done),
{
    let n = jobs.len();
    if n == 0 {
        return;
    }

    // The sequential path emits per job and holds nothing, which is already the
    // bounded-memory ideal. Kept separate rather than folded into the pool:
    // `-j1` is the production default and deserves no threads at all.
    if workers <= 1 {
        for job in &jobs {
            let mut buf = make_buffer();
            let outcome = work(job, &mut buf);
            emit(Done { index: job.index, buffer: buf, outcome });
        }
        return;
    }

    let next = AtomicUsize::new(0);
    let state = Mutex::new(Shared {
        slots: (0..n).map(|_| None).collect(),
        ready: VecDeque::new(),
        held: 0,
        finished: 0,
    });
    let wake = Condvar::new();

    std::thread::scope(|scope| {
        for _ in 0..workers.min(n) {
            let (next, state, wake) = (&next, &state, &wake);
            let (jobs, work, make_buffer) = (&jobs, &work, &make_buffer);
            scope.spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        return;
                    }
                    let mut buf = make_buffer();
                    let outcome = work(&jobs[i], &mut buf);
                    let bytes = buf.as_slice().len();
                    let done = Done { index: jobs[i].index, buffer: buf, outcome };

                    let mut st = state.lock().unwrap();
                    st.slots[i] = Some(done);
                    st.ready.push_back(i);
                    st.held += bytes;
                    st.finished += 1;
                    drop(st);
                    wake.notify_all();
                }
            });
        }

        // The collector. Runs on the calling thread so `emit` needs to be
        // neither `Send` nor `Sync`, and so ordering decisions live in exactly
        // one place.
        let mut head = 0usize;
        let mut emitted = vec![false; n];
        let mut emitted_count = 0usize;
        let mut batch: Vec<Done> = Vec::new();

        while emitted_count < n {
            {
                let mut st = state.lock().unwrap();
                loop {
                    if sorted {
                        // Drain the in-order prefix: everything from `head`
                        // onwards that has landed.
                        while head < n {
                            if emitted[head] {
                                head += 1;
                                continue;
                            }
                            match st.slots[head].take() {
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
                            for (i, flag) in emitted.iter_mut().enumerate().skip(head) {
                                if let Some(d) = st.slots[i].take() {
                                    st.held -= d.buffer.as_slice().len();
                                    *flag = true;
                                    batch.push(d);
                                }
                            }
                        }
                    } else {
                        while let Some(i) = st.ready.pop_front() {
                            if let Some(d) = st.slots[i].take() {
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
                    // outstanding and a worker will notify.
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
                emit(d);
            }
        }
    });
}
