//! Bounded worker pool. Concurrency lives at the archive boundary and nowhere
//! else, because gzip has no index: member N requires inflating 1..N-1.
//!
//! # Ordering
//!
//! Output stays in argument order by default: these archives are days, and
//! argument order is chronological order. That is achieved by holding an
//! archive's output until every earlier archive has been emitted — **not** by
//! joining all workers first. A finished job whose predecessors are already
//! out is written and freed immediately, so nothing waits on the slowest
//! archive to be handed to `emit`.
//!
//! Sorted output is in order, always. There used to be an escape valve that
//! flushed parked output out of order once it passed `spill_bytes`, and it
//! fired in ordinary `-j8` runs with an exit code of 0. The backpressure below
//! replaced it: nothing is ever emitted out of turn.
//!
//! `--no-sort` keeps its own promise: one archive's output is one unbroken
//! block. Which archive comes first is up to the run.
//!
//! # What bounds memory
//!
//! A job does not write into one buffer that grows with its match volume. It
//! writes into an [`Output`], which seals a *segment* every [`SEGMENT`] bytes
//! and hands it to the scheduler. What happens to a sealed segment depends on
//! whose turn it is to write:
//!
//! - **The job whose turn it is streams.** In sorted mode that is the job at
//!   the head of the order; at `-j1` it is always the one job running. Its
//!   segments go to `emit` as they are sealed, so a 4 MB archive that matches
//!   1.6 GB of lines costs a few segments, not 1.6 GB. It waits only for the
//!   collector to keep up, never more than `AHEAD` segments ahead of it.
//! - **Every other job buffers, up to a budget.** Its segments are parked in
//!   its slot and counted in `held`. Once what is parked passes the budget,
//!   `spill_bytes`, a job that seals another segment *waits*: until it is its
//!   turn, or until enough has been emitted. In sorted mode the second half
//!   of the budget is kept for the jobs nearest their turn (`over_budget`).
//!   Workers between jobs wait on the same budget before they claim another.
//!
//! So peak output memory is about `spill_bytes`, plus one segment per worker
//! being filled, plus what the collector is writing. Three smaller things
//! keep the rest flat:
//!
//! 1. **Buffer recycling.** Segments are pooled: the collector `clear()`s one
//!    after emitting it and a worker takes it again. A segment never grows
//!    past [`SEGMENT`] unless a single write is larger, so there is no realloc
//!    chain to an archive's whole match volume and nothing for the allocator
//!    to fragment. At `workers == 1` exactly one buffer exists for the run.
//! 2. **Per-worker state.** `make_worker` is called once per worker thread, not
//!    once per job — see [`run`].
//! 3. **Capped diagnostics.** An archive's errors and notes are capped where
//!    they are collected (`archive::Outcome`), so an archive in which every
//!    member fails cannot grow a list without bound either.
//!
//! # Why the backpressure cannot deadlock
//!
//! A worker that waits *inside* a job holds that job's output, so the wait
//! needs an argument, and this is it.
//!
//! *Jobs are claimed in emission order.* The claim counter indexes `order`,
//! the same vector the collector's head walks, so every job ahead of a
//! claimed job has been claimed too — for any `Job::index` a caller passes,
//! not only for `main`'s `0..n`.
//!
//! *The job at the head never waits for a turn.* It is the one whose turn it
//! is. It waits only while more than `AHEAD` of its own segments are queued,
//! and the collector takes those on every pass: the collector itself blocks
//! on nothing but `emit` and on `wake`, which every deposit notifies.
//!
//! So take any worker parked inside job `p`, not at the head. The head `h` is
//! ahead of `p`, hence claimed, hence either running — it finishes, or parks
//! on its own queue, which the collector drains — or finished, and the
//! collector takes it and moves the head on, notifying `space`. The head only
//! moves forward, and each move is one job nearer `p`; when it reaches `p`,
//! `p` is the job that streams. A claimed job that never deposits would break
//! the chain, which is what the [`Completion`] guard is for: a worker that
//! unwinds mid-job leaves an error in its slot, and the head moves past it.
//!
//! A worker waiting *between* jobs holds nothing, and what it waits for is
//! always in motion: if the head is unclaimed, nothing after it is claimed
//! either, so nothing is parked and only the collector's batch is counted,
//! which `recycle` gives back.
//!
//! `--no-sort` has no head, since finished jobs are taken as they land.
//! There the turn is a token, `streamer`, and the job that may take it is
//! the lowest one still unfinished — the *front*, which stands where the
//! head does in the argument above. The front never waits for a turn: when
//! the budget would make it wait, it takes the token instead, and holds it
//! until it finishes, so its block stays unbroken. A job parked behind it
//! waits for one of two things, and `space` is notified at both: the token
//! given back, when the collector takes the streaming job's last piece; or
//! the front finishing without ever streaming, when the collector takes
//! that. Either moves the front up, and the job that is now the front is
//! awake to take the token.
//!
//! Every one of these waits also ends on `stop`, set under the lock.
//!
//! # Design
//!
//! One `Mutex` guards the slots, the buffer pool, and a little bookkeeping;
//! `wake` wakes the collector and `space` wakes waiting workers, both on that
//! mutex, so a check-and-wait is atomic against the change it waits for.
//! Workers pull the next job with a `fetch_add`, do the work outside the lock,
//! and take the lock only to move a segment or the finished job into the slot.
//!
//! The calling thread is the collector. It calls `emit` *outside* the lock so
//! a slow stdout — a pipe into `head(1)`, say — never stalls a worker that
//! still has room. It is the only thread that writes, which is why `emit`
//! needs to be neither `Send` nor `Sync`, and why one job's segments can never
//! interleave with another's: the collector takes pieces from one job until
//! that job's last.
//!
//! # The load gate
//!
//! [`run_gated`] takes a [`Gate`]: how many workers may be inside a job at
//! once, read before every claim and free to move while the run goes on. That
//! is what makes `--load-limit` adaptive — every `-j` worker is started, and
//! the ones the gate leaves out wait between archives, holding nothing.
//!
//! Places at the gate are counted, not assigned to workers: a gate of 1 admits
//! whichever worker gets there first, never "worker 0". An assigned place
//! would belong to a worker that might be dead, with the rest waiting behind
//! it for good. A place is released by a drop guard, so a worker that unwinds
//! gives its place back too.
//!
//! A worker held at the gate polls rather than waiting on a condvar. The gate
//! is moved by a thread outside the pool that knows nothing of its locks, and
//! a poll cannot miss a wakeup: it sees a raised gate, a stop, or the end of
//! the job list within [`GATE_POLL`], whoever caused it. It costs nothing when
//! no gate is given, and a few wakeups a second while one is closed.
//!
//! The gate also reports back: [`Gate::busy`] is how many workers are inside a
//! job and not parked, which is trg's own share of the load average.
//!
//! # Panics and early stop
//!
//! A panicking job must not be able to hang the process — during an incident a
//! hang looks like a slow search and gets waited on, which is strictly worse
//! than a crash. Four independent mechanisms guarantee it cannot, one per place
//! a thread can leave unexpectedly:
//!
//! - [`catch_unwind`](std::panic::catch_unwind) around the call to `work`,
//!   which turns a panic into an ordinary error on that archive, and a second
//!   one around `make_worker`, which is likewise the caller's closure.
//! - a `Completion` drop guard that finishes the job with an error and wakes
//!   everyone even if a worker unwinds anyway, once it has claimed a job.
//! - a `WorkerLife` drop guard covering the rest of the thread's life —
//!   everything before and between jobs, where `Completion` does not exist yet.
//!   When the last worker goes with jobs still unclaimed, the collector stops
//!   waiting and reports those archives as unread, so the run is a 2.
//! - a `Teardown` drop guard on the collector, releasing workers parked on
//!   backpressure when it leaves, including by unwinding out of `emit`.
//!
//! Each guards a different exit; none of them is redundant with the others, and
//! the module has hit this failure shape four times, so a new path out of a
//! thread needs a guard rather than an argument that it cannot be taken.
//!
//! `emit` returns `io::Result`, and the first error stops the run: no further
//! job is started, jobs in flight give up at their next read or segment
//! ([`Control::stopped`]), and [`run`] returns the error. That is what makes
//! `trg ... | head -5` end when `head` does instead of grinding through a
//! month of archives writing into a closed pipe.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::archive::Outcome;

/// How long a worker held at the gate sleeps between looks. Short against an
/// archive, long against a busy loop.
const GATE_POLL: Duration = Duration::from_millis(50);

/// How long a worker that could not open its archive for want of a file
/// descriptor waits before it tries again.
const FD_POLL: Duration = Duration::from_millis(20);

/// The size at which an [`Output`] seals what it holds and starts afresh.
/// Small against any budget worth setting, large against the cost of taking
/// the lock once per segment. A power of two, so a pooled buffer that has
/// grown to it by doubling holds exactly one segment.
pub const SEGMENT: usize = 1 << 20;

/// How many segments the job that is streaming may queue ahead of the
/// collector before it waits for it.
const AHEAD: usize = 4;

pub struct Job {
    pub index: usize,
    pub path: std::path::PathBuf,
}

/// One piece of a job's output, on its way to `emit`: all of it for a job
/// that printed little, otherwise one of several, in order and with no other
/// job's between them.
pub struct Done {
    pub index: usize,
    pub buffer: termcolor::Buffer,
    /// What the job concluded. Empty on a `part`: the job is still running.
    pub outcome: Outcome,
    /// More of this job's output follows. The last piece of a job, and the
    /// only one that carries its outcome, has `part == false`.
    pub part: bool,
    /// An earlier piece of this job has been emitted already.
    pub continued: bool,
    /// On a job's first piece: whether its output starts with a line and not
    /// with a lone `binary file matches`. Known when the first bytes are
    /// released, which is when the caller has to decide about a `--`.
    pub leads_with_line: bool,
}

/// How the pool runs, as opposed to what it runs.
pub struct Config {
    /// Archives searched concurrently. `1` takes a dedicated sequential path.
    pub workers: usize,
    /// Emit in [`Job::index`] order rather than completion order.
    pub sorted: bool,
    /// How many bytes of output may wait for their turn. A job that is not
    /// next to be written pauses once this much is waiting, and resumes when
    /// it is its turn or the wait has drained: output past the budget spills
    /// through to `emit` in order, never around it. See the module docs.
    pub spill_bytes: usize,
}

/// How many workers may be inside a job at once, and how many are.
pub struct Gate {
    /// The gate itself. Values below 1 count as 1.
    pub open: AtomicUsize,
    busy: Arc<AtomicUsize>,
}

impl Gate {
    pub fn new(open: usize) -> Self {
        Self { open: AtomicUsize::new(open), busy: Arc::default() }
    }

    /// Workers inside a job and not parked on backpressure: the ones that are
    /// running or reading the disk, which is what a load average counts.
    pub fn busy(&self) -> usize {
        self.busy.load(Ordering::Relaxed)
    }
}

/// What a job is told about the run it is part of, and tells it.
///
/// Cheap to clone, and separate from the [`Output`] it comes from because the
/// output is borrowed by a printer for the whole of a search, while the search
/// still needs to ask whether to go on.
#[derive(Clone)]
pub struct Control {
    stop: Arc<AtomicBool>,
    /// Workers inside a job and not parked.
    busy: Arc<AtomicUsize>,
    /// Of those, the ones waiting for a file descriptor.
    starved: Arc<AtomicUsize>,
    /// Jobs claimed so far in the run.
    claimed: Arc<AtomicUsize>,
    /// Held by the one starved worker that is trying its archive again.
    fd_turn: Arc<Mutex<()>>,
    lead: Arc<AtomicBool>,
}

impl Control {
    /// For a walk outside any run: never stopped, and nobody else to wait for.
    pub fn detached() -> Self {
        Self {
            stop: Arc::default(),
            busy: Arc::default(),
            starved: Arc::default(),
            claimed: Arc::default(),
            fd_turn: Arc::default(),
            lead: Arc::default(),
        }
    }

    /// For the one job of a sequential run.
    fn alone() -> Self {
        let c = Self::detached();
        c.busy.store(1, Ordering::Relaxed);
        c
    }

    /// The run is over — a failed write, a closed pipe — and whatever this job
    /// has left to read is not wanted. Read by the archive walk on every read,
    /// so a job stops within one buffer and not at the end of its archive.
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// The job's output starts with a line. To be said before that line is
    /// written: the first segment may leave while it is.
    pub fn leads_with_line(&self) {
        self.lead.store(true, Ordering::Relaxed);
    }

    /// Call `open` until it stops failing for want of a file descriptor, or
    /// until waiting cannot help.
    ///
    /// `-j200` under `ulimit -n 64` cannot hold 200 archives open. Without
    /// this the workers past the limit fail at once, take the next job, fail
    /// again, and race through the list: nearly every archive reported
    /// unread with "Too many open files", while a handful of workers do all
    /// the reading. Waiting helps as long as some other worker is inside a
    /// job and getting on with it, since it closes its archive when it is
    /// done. It cannot help once every worker inside a job is waiting like
    /// this one or parked on backpressure — parked workers hold their
    /// archives open until this one, perhaps, lets them go on — so that is
    /// where it gives up, and the error stands.
    ///
    /// "Nobody else is at work" has to be true across a whole attempt to
    /// count. An open takes several descriptors one after another, and two
    /// workers trying at once can each hold the one the other is missing, so
    /// waiting workers take turns; and a worker that claimed a job during
    /// the attempt may have been part-way through its own first open, so an
    /// attempt during which a job was claimed is tried again.
    pub fn retry_for_fds<T>(&self, mut open: impl FnMut() -> anyhow::Result<T>) -> anyhow::Result<T> {
        let mut r = open();
        if !r.as_ref().is_err_and(out_of_fds) {
            return r;
        }
        let others = || self.busy.load(Ordering::Acquire) > self.starved.load(Ordering::Acquire);
        self.starved.fetch_add(1, Ordering::AcqRel);
        loop {
            std::thread::sleep(FD_POLL);
            // Poisoned or not: the lock guards nothing but the turn.
            let _turn = self.fd_turn.lock();
            let before = (others(), self.claimed.load(Ordering::Acquire));
            r = open();
            let after = (others(), self.claimed.load(Ordering::Acquire));
            let alone = !before.0 && !after.0 && before.1 == after.1;
            if !r.as_ref().is_err_and(out_of_fds) || self.stopped() || alone {
                break;
            }
        }
        self.starved.fetch_sub(1, Ordering::AcqRel);
        r
    }
}

/// Whether an open failed because the process (`EMFILE`) or the system
/// (`ENFILE`) has no file descriptor left to give.
fn out_of_fds(e: &anyhow::Error) -> bool {
    e.downcast_ref::<io::Error>()
        .and_then(io::Error::raw_os_error)
        .is_some_and(|c| c == libc::EMFILE || c == libc::ENFILE)
}

/// The error a write gets once the run has stopped. Never shown: a stopped
/// job's verdict is discarded, and the error that stopped the run is the one
/// `run` returns.
fn stopped() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "the run has stopped")
}

/// Where a job writes. A `termcolor::Buffer` that does not grow with the
/// job's match volume: every [`SEGMENT`] bytes it seals what it holds and
/// hands it on, to `emit` if it is this job's turn to write and to its slot
/// if not. See "What bounds memory" in the module docs.
///
/// Segments are cut at write boundaries, not line boundaries. That is safe
/// because nothing else is written between two pieces of one job, so the
/// bytes that reach stdout are the bytes the job wrote, colour sequences
/// included, whatever the cuts.
pub struct Output<'s> {
    buf: termcolor::Buffer,
    /// Bytes written to `buf` since it was last sealed. Counted here so the
    /// check on every write is an add and a compare: the printer writes a
    /// line in half a dozen pieces, and a hundred million of those is where
    /// a search that matches everything spends its time.
    len: usize,
    /// What `len` may reach before the segment is sealed. Zero once the run
    /// has stopped, so that every write from then on takes the slow path and
    /// fails there.
    limit: usize,
    index: usize,
    /// A segment of this job has been handed on.
    continued: bool,
    /// The run has stopped, and every write from here on fails.
    dead: bool,
    ctl: &'s Control,
    via: Via<'s>,
}

enum Via<'s> {
    /// The sequential run: straight to `emit`, on this thread.
    Direct { emit: &'s mut dyn FnMut(&Done) -> io::Result<()>, failed: Option<io::Error> },
    /// The pool: into the job's slot, for the collector.
    Pool { pool: &'s Pool<'s>, rank: usize, pos: usize },
}

impl<'s> Output<'s> {
    fn new(buf: termcolor::Buffer, segment: usize, index: usize, ctl: &'s Control, via: Via<'s>) -> Self {
        Output { len: buf.len(), buf, limit: segment, index, continued: false, dead: false, ctl, via }
    }

    /// The run's side of this job. See [`Control`].
    pub fn control(&self) -> Control {
        self.ctl.clone()
    }

    /// Off the fast path: a write of `more` bytes would pass the limit.
    ///
    /// Sealed *before* the write that would pass the segment size, so a
    /// pooled buffer is never grown past it and doubled. A single write
    /// larger than a segment goes into an empty one whole.
    #[cold]
    fn make_room(&mut self) -> io::Result<()> {
        if self.dead {
            return Err(stopped());
        }
        if self.len > 0 {
            self.seal()?;
        }
        Ok(())
    }

    /// What the job wrote last, and whether more went before it.
    fn into_last(self) -> (termcolor::Buffer, bool, Via<'s>) {
        (self.buf, self.continued, self.via)
    }

    /// The run has stopped: fail this write and every later one.
    fn die(&mut self) -> io::Error {
        self.dead = true;
        self.limit = 0;
        stopped()
    }

    /// Hand on what is held and start a fresh segment, waiting first if this
    /// job has to. An error means the run has stopped.
    fn seal(&mut self) -> io::Result<()> {
        let lead = self.ctl.lead.load(Ordering::Relaxed);
        self.len = 0;
        match &mut self.via {
            Via::Direct { emit, failed } => {
                let piece = Done {
                    index: self.index,
                    buffer: std::mem::replace(&mut self.buf, termcolor::Buffer::no_color()),
                    outcome: Outcome::default(),
                    part: true,
                    continued: self.continued,
                    leads_with_line: !self.continued && lead,
                };
                let r = emit(&piece);
                self.buf = piece.buffer;
                self.buf.clear();
                self.continued = true;
                if let Err(e) = r {
                    // Kept for `run` to return: it is the caller's to tell a
                    // closed pipe from a full disk.
                    *failed = Some(e);
                    self.ctl.stop.store(true, Ordering::Release);
                    return Err(self.die());
                }
                Ok(())
            }
            Via::Pool { pool, rank, pos } => {
                let (pool, rank, pos) = (*pool, *rank, *pos);
                let full = std::mem::replace(&mut self.buf, termcolor::Buffer::no_color());
                let len = full.len();
                let mut st = pool.state.lock().unwrap();
                let slot = &mut st.slots[pos];
                if !self.continued {
                    slot.lead = lead;
                }
                slot.parts.push_back(full);
                slot.bytes += len;
                st.held += len;
                self.continued = true;
                // The collector, in case it is this job's turn.
                pool.wake.notify_all();

                // The two waits of the module docs. Not busy while parked:
                // a parked worker is asleep, and is no part of the load.
                let mut parked = false;
                let stopped_now = loop {
                    if pool.stop.load(Ordering::Acquire) {
                        break true;
                    }
                    let wait = if pool.my_turn(&mut st, rank, pos) {
                        st.slots[pos].bytes > pool.ahead
                    } else {
                        pool.over_budget(&mut st, rank)
                    };
                    if !wait {
                        break false;
                    }
                    if !parked {
                        parked = true;
                        pool.busy.fetch_sub(1, Ordering::AcqRel);
                    }
                    st = pool.space.wait(st).unwrap();
                };
                if parked {
                    pool.busy.fetch_add(1, Ordering::AcqRel);
                }
                if stopped_now {
                    return Err(self.die());
                }
                let fresh = st.pool.pop();
                drop(st);
                // Outside the lock: `make_buffer` is the caller's closure.
                self.buf = fresh.unwrap_or_else(|| (pool.make_buffer)());
                Ok(())
            }
        }
    }
}

impl io::Write for Output<'_> {
    #[inline]
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if self.len + b.len() > self.limit {
            self.make_room()?;
        }
        self.len += b.len();
        self.buf.write_all(b)?;
        Ok(b.len())
    }

    #[inline]
    fn write_all(&mut self, b: &[u8]) -> io::Result<()> {
        if self.len + b.len() > self.limit {
            self.make_room()?;
        }
        self.len += b.len();
        self.buf.write_all(b)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl termcolor::WriteColor for Output<'_> {
    fn supports_color(&self) -> bool {
        self.buf.supports_color()
    }

    fn supports_hyperlinks(&self) -> bool {
        self.buf.supports_hyperlinks()
    }

    // The colour sequences are bytes in the buffer like any others, written
    // by termcolor, so the count is read back after each. They never seal a
    // segment themselves: the next write of text does.
    fn set_color(&mut self, spec: &termcolor::ColorSpec) -> io::Result<()> {
        let r = self.buf.set_color(spec);
        self.len = self.buf.len();
        r
    }

    fn set_hyperlink(&mut self, link: &termcolor::HyperlinkSpec) -> io::Result<()> {
        let r = self.buf.set_hyperlink(link);
        self.len = self.buf.len();
        r
    }

    fn reset(&mut self) -> io::Result<()> {
        let r = self.buf.reset();
        self.len = self.buf.len();
        r
    }

    fn is_synchronous(&self) -> bool {
        self.buf.is_synchronous()
    }
}

/// One job's place in the pool.
#[derive(Default)]
struct Slot {
    /// Sealed segments the collector has not taken yet, oldest first.
    parts: VecDeque<termcolor::Buffer>,
    /// The job's last piece and its verdict, once it has finished.
    last: Option<(termcolor::Buffer, Outcome)>,
    /// Bytes in `parts` and `last`.
    bytes: usize,
    /// Whether the job's output starts with a line, as of its first segment.
    lead: bool,
    /// The collector has taken a piece of this job.
    begun: bool,
    /// The collector has taken this job's last piece.
    taken: bool,
}

/// Everything the workers and the collector share. The lock is held only for
/// the moves in and out of `slots` and `pool`.
struct Shared {
    /// One slot per job position.
    slots: Vec<Slot>,
    /// Positions in completion order, for `--no-sort`.
    ready: VecDeque<usize>,
    /// Output buffers not currently in use, kept so their allocations are
    /// reused instead of being rebuilt per segment.
    pool: Vec<termcolor::Buffer>,
    /// Bytes parked in `slots`, waiting for their turn.
    held: usize,
    /// Bytes the collector has taken out of `slots` but not yet emitted and
    /// recycled. Counted separately and added to `held` for backpressure,
    /// because taking moves bytes out of `slots` without freeing anything —
    /// missing this is what let live buffers track the job count.
    batch_bytes: usize,
    /// Jobs finished so far, so the collector knows when to stop waiting.
    finished: usize,
    /// Worker threads still running. Decremented by [`WorkerLife`] on every
    /// exit path, normal or unwinding, so that "nobody is left to finish the
    /// outstanding jobs" is a state the collector can observe rather than a
    /// wait it never wakes from.
    live_workers: usize,
    /// How far along `order` the run is. In sorted mode the collector moves
    /// it, and the job there is the one whose turn it is to write. Under
    /// `--no-sort` it is the lowest job still unfinished: see `Pool::front`.
    head: usize,
    /// `--no-sort`: the position of the job that holds the turn, if one does.
    streamer: Option<usize>,
}

/// What a worker needs of the pool to hand a segment on.
struct Pool<'s> {
    state: &'s Mutex<Shared>,
    wake: &'s Condvar,
    space: &'s Condvar,
    stop: &'s AtomicBool,
    busy: &'s AtomicUsize,
    make_buffer: &'s (dyn Fn() -> termcolor::Buffer + Sync),
    /// Job positions in the order they are emitted, and claimed.
    order: &'s [usize],
    sorted: bool,
    /// The size of a segment: [`SEGMENT`], or a smaller budget.
    segment: usize,
    /// `Config::spill_bytes`.
    budget: usize,
    /// How many bytes the streaming job may queue before it waits.
    ahead: usize,
}

impl Pool<'_> {
    /// How far along `order` the run has got: the rank of the job whose turn
    /// it is, or will be first.
    ///
    /// In sorted mode that is the collector's head. Under `--no-sort` nothing
    /// has a turn until the budget is spent, and then the lowest job still
    /// unfinished gets it; `head` is moved up to it here, past the jobs that
    /// have finished, since the collector takes those in whatever order they
    /// land.
    fn front(&self, st: &mut Shared) -> usize {
        if !self.sorted {
            while st.head < self.order.len() && {
                let slot = &st.slots[self.order[st.head]];
                slot.taken || slot.last.is_some()
            } {
                st.head += 1;
            }
        }
        st.head
    }

    /// Whether a job that is not the one writing has to wait before it
    /// buffers more.
    ///
    /// The budget is in two halves. The first is anyone's: nobody waits
    /// until half the budget is waiting, which is every run but the ones
    /// that match a great deal. The second half goes to the jobs whose turn
    /// comes soonest: a job may go on while it and the jobs ahead of it
    /// together hold no more than half the budget. What a far job holds
    /// stays held the longest, so shared alike, `-j8` gave each of seven jobs
    /// a seventh of the budget and left the next in line as unfinished as the
    /// last; this way the next job in line is usually whole by the time the
    /// one ahead of it is done, and goes out at once. Measured on 40
    /// archives of 30 MB of matches each at `-j8`: 1.0 s shared alike, 0.65 s
    /// like this, 1.5 s at `-j1`. Never more than the budget, since the
    /// second test implies the whole of it.
    fn over_budget(&self, st: &mut Shared, rank: usize) -> bool {
        let waiting = st.held + st.batch_bytes;
        let half = self.budget / 2;
        if waiting <= half {
            return false;
        }
        let front = self.front(st).min(rank);
        let ahead: usize = self.order[front..=rank].iter().map(|&p| st.slots[p].bytes).sum();
        ahead > half || waiting > self.budget
    }

    /// Whether the job at `rank` in the order, position `pos`, is the one
    /// that writes now.
    ///
    /// In sorted mode, the job at the head. Under `--no-sort` this is also
    /// where the turn is taken: by the lowest job still unfinished, when it
    /// would otherwise have to wait. It keeps the turn until it is done, so
    /// its output stays in one block.
    fn my_turn(&self, st: &mut Shared, rank: usize, pos: usize) -> bool {
        if self.sorted {
            return st.head == rank;
        }
        if let Some(s) = st.streamer {
            return s == pos;
        }
        if self.front(st) == rank && self.over_budget(st, rank) {
            st.streamer = Some(pos);
            self.wake.notify_all();
            return true;
        }
        false
    }
}

/// Finishes the job with an error on drop, so that a worker which unwinds
/// after claiming a job still completes it.
///
/// Without this, a panicking worker would leave `finished` short of `n`
/// forever: the collector would wait on a notification that never comes, and
/// `thread::scope` would never return. Belt and braces alongside the
/// `catch_unwind` in [`run_job`], which already stops a panic in `work` itself;
/// this covers everything else on the path.
///
/// It leaves a verdict in the slot, not only a count. The head of the order
/// moves when a job's last piece is taken, and workers parked behind it wait
/// for that: a claimed job with nothing in its slot would hold them there for
/// good. The verdict is an error, so the run is a 2.
struct Completion<'a> {
    state: &'a Mutex<Shared>,
    wake: &'a Condvar,
    space: &'a Condvar,
    job: &'a Job,
    pos: usize,
    sorted: bool,
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
            let mut o = Outcome::default();
            o.errors.push(format!(
                "{}: the worker searching it died; not read to its end",
                crate::source::display(&self.job.path)
            ));
            // An empty buffer of any kind will do: nothing is written from it.
            st.slots[self.pos].last = Some((termcolor::Buffer::no_color(), o));
            if !self.sorted {
                st.ready.push_back(self.pos);
            }
            st.finished += 1;
        }
        self.wake.notify_all();
        self.space.notify_all();
    }
}

/// Decrements the live-worker count and wakes the collector on drop.
///
/// [`Completion`] covers a worker that unwinds *after* it has claimed a job.
/// This covers the rest of the thread's life, which [`Completion`] cannot: it
/// is constructed as the thread's first act, before `make_worker()` and before
/// the first `state.lock()`, so a panic in either — or in `space.wait()` on a
/// poisoned lock — still tells the collector that this worker is gone.
///
/// Without it the collector waits at `wake.wait` for a notification no living
/// thread will send, and `thread::scope` never returns: a hang, which during an
/// incident looks like a slow search and gets waited on. This is the third
/// instance of that shape in this module, after the panicking worker and the
/// collector unwinding out of `emit`, so it is guarded structurally rather than
/// by arguing that the current caller does not panic — `run` is public API and
/// `make_worker` is the caller's closure.
struct WorkerLife<'a> {
    state: &'a Mutex<Shared>,
    wake: &'a Condvar,
}

impl Drop for WorkerLife<'_> {
    fn drop(&mut self) {
        // Never `unwrap`: same discipline as `Completion` — this `Drop` runs
        // during an unwind, and panicking there aborts the process.
        if let Ok(mut st) = self.state.lock() {
            st.live_workers -= 1;
        }
        self.wake.notify_all();
    }
}

/// Releases every worker parked on backpressure when the collector leaves,
/// *including* when it leaves by unwinding out of `emit`.
///
/// The exact mirror of [`Completion`] at the other end of the pipe. Without it,
/// a panic in `emit` skips the teardown, workers stay parked on `space`, and
/// `thread::scope` blocks forever joining them — a hang, which is the one
/// outcome worse than a crash. Fixing that on the worker side and not the
/// collector side would not be defensible.
struct Teardown<'a> {
    state: &'a Mutex<Shared>,
    space: &'a Condvar,
    wake: &'a Condvar,
    stop: &'a AtomicBool,
}

impl Drop for Teardown<'_> {
    fn drop(&mut self) {
        // Never `unwrap` here: this `Drop` runs during an unwind, and a panic
        // inside it would abort the process. Same discipline as `Completion`.
        if let Ok(mut st) = self.state.lock() {
            // The `Done`s still in the collector's batch are dropped without
            // passing through `recycle`, so their bytes would be counted
            // against the backpressure predicate forever. Zeroing here makes
            // the predicate correct rather than merely short-circuited, so the
            // wakeup below does not depend on `stop` alone.
            st.batch_bytes = 0;
            // Stored while the lock is held so the mutex's release/acquire edge
            // carries it too; the `Release` ordering covers the lock-free check
            // at the top of the worker loop.
            self.stop.store(true, Ordering::Release);
        } else {
            self.stop.store(true, Ordering::Release);
        }
        self.space.notify_all();
        self.wake.notify_all();
    }
}

/// One place at the gate, given back on drop — unwinding included, so a dying
/// worker cannot keep a place the others are waiting for.
struct Permit<'a>(&'a AtomicUsize);

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// One worker counted as busy, uncounted on drop.
struct Busy<'a>(&'a AtomicUsize);

impl<'a> Busy<'a> {
    fn new(n: &'a AtomicUsize) -> Self {
        n.fetch_add(1, Ordering::AcqRel);
        Self(n)
    }
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Wait for a place at the gate. `None` means the worker should exit instead:
/// the run has been stopped, or every job is already claimed. Checking those
/// here, and not only after admission, is what keeps a held-back worker from
/// outliving the run.
fn admit<'a>(
    gate: &AtomicUsize,
    inside: &'a AtomicUsize,
    stop: &AtomicBool,
    next: &AtomicUsize,
    n: usize,
) -> Option<Permit<'a>> {
    loop {
        if stop.load(Ordering::Acquire) || next.load(Ordering::Relaxed) >= n {
            return None;
        }
        // Clamped: a gate of 0 would admit nobody and the run would never end.
        let cap = gate.load(Ordering::Relaxed).max(1);
        let now = inside.load(Ordering::Acquire);
        if now < cap {
            if inside
                .compare_exchange_weak(now, now + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(Permit(inside));
            }
            continue;
        }
        std::thread::sleep(GATE_POLL);
    }
}

/// Call `work`, converting a panic into an error on that archive.
///
/// What the job held unsealed is cleared on panic: a job that died mid-write
/// may have left a partial line behind, and half a log line in the middle of
/// ordered output is worse than none.
///
/// This is a no-op under `panic = "abort"`, which the release profile sets. It
/// is still worth having: an abort is a crash, and a crash is a diagnosable
/// outcome. The failure mode being prevented is the *hang*, and the
/// `Completion` guard prevents that on both profiles.
fn run_job<C, F>(job: &Job, ctx: &mut C, out: &mut Output<'_>, work: &F) -> Outcome
where
    F: Fn(&Job, &mut C, &mut Output<'_>) -> Outcome,
{
    // `AssertUnwindSafe` is honest here rather than a silencer: everything
    // reachable is either this job's own scratch state, which is discarded on
    // the panic path, or the job's slot, which holds whole segments only.
    let called = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(job, ctx, out)));
    match called {
        Ok(o) => o,
        Err(_) => {
            out.buf.clear();
            out.len = 0;
            let mut o = Outcome::default();
            o.errors.push(format!(
                "{}: panicked while searching; skipped",
                crate::source::display(&job.path)
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
/// recycled afterwards. It is called once per piece: once for a job that
/// printed less than a segment, and otherwise several times, in order, with
/// [`Done::part`] set on all but the last. The first `Err` it returns stops
/// the run: no further job is started, and that error is returned.
pub fn run<C, F, E>(
    jobs: Vec<Job>,
    cfg: Config,
    make_buffer: impl Fn() -> termcolor::Buffer + Send + Sync,
    make_worker: impl Fn() -> C + Send + Sync,
    work: F,
    emit: E,
) -> io::Result<()>
where
    F: Fn(&Job, &mut C, &mut Output<'_>) -> Outcome + Send + Sync,
    E: FnMut(&Done) -> io::Result<()>,
{
    run_with_spawn_hook(jobs, cfg, None, make_buffer, make_worker, work, emit, |_| Ok(()))
}

/// [`run`], with at most `gate.open` workers inside a job at any moment. The
/// gate is re-read before every claim, so it may be moved during the run;
/// values below 1 count as 1. See "The load gate" in the module docs.
///
/// At `workers == 1` the gate has nothing to hold back and is ignored.
pub fn run_gated<C, F, E>(
    jobs: Vec<Job>,
    cfg: Config,
    gate: &Gate,
    make_buffer: impl Fn() -> termcolor::Buffer + Send + Sync,
    make_worker: impl Fn() -> C + Send + Sync,
    work: F,
    emit: E,
) -> io::Result<()>
where
    F: Fn(&Job, &mut C, &mut Output<'_>) -> Outcome + Send + Sync,
    E: FnMut(&Done) -> io::Result<()>,
{
    run_with_spawn_hook(jobs, cfg, Some(gate), make_buffer, make_worker, work, emit, |_| Ok(()))
}

/// The sequential run: no threads, no lock, one worker context and one buffer.
/// Kept separate rather than folded into the pool: `-j1` is the production
/// default and deserves no threads at all. It is also what a pool falls back
/// to when the OS will not give it a single thread.
///
/// The one job running is always the one whose turn it is, so its segments go
/// straight to `emit`.
fn run_sequential<C, F, E>(
    jobs: &[Job],
    segment: usize,
    make_buffer: &impl Fn() -> termcolor::Buffer,
    make_worker: &impl Fn() -> C,
    work: &F,
    emit: &mut E,
) -> io::Result<()>
where
    F: Fn(&Job, &mut C, &mut Output<'_>) -> Outcome,
    E: FnMut(&Done) -> io::Result<()>,
{
    let ctl = Control::alone();
    let mut ctx = make_worker();
    let mut buf = make_buffer();
    for job in jobs {
        buf.clear();
        ctl.lead.store(false, Ordering::Relaxed);
        let mut out = Output::new(buf, segment, job.index, &ctl, Via::Direct { emit: &mut *emit, failed: None });
        let outcome = run_job(job, &mut ctx, &mut out, work);
        let (last, continued, via) = out.into_last();
        let failed = match via {
            Via::Direct { failed, .. } => failed,
            Via::Pool { .. } => None,
        };
        let mut done = Done {
            index: job.index,
            buffer: last,
            leads_with_line: !continued && outcome.leads_with_line,
            outcome,
            part: false,
            continued,
        };
        if let Some(e) = failed {
            // The job's verdict still goes to `emit`, with nothing to write:
            // what it found unreadable before the write failed is still true.
            done.buffer.clear();
            let _ = emit(&done);
            return Err(e);
        }
        let r = emit(&done);
        buf = done.buffer;
        r?;
        // A single write far past a segment — one enormous line — should not
        // stay reserved for the rest of the run.
        if buf.len() > segment {
            buf = make_buffer();
        }
    }
    Ok(())
}

/// One worker thread of the pool, from its first act to its last.
#[allow(clippy::too_many_arguments)]
fn worker<C, F>(
    pool: &Pool<'_>,
    jobs: &[Job],
    order: &[usize],
    next: &AtomicUsize,
    gate: Option<&Gate>,
    inside: &AtomicUsize,
    ctl: Control,
    make_worker: &impl Fn() -> C,
    work: &F,
) where
    F: Fn(&Job, &mut C, &mut Output<'_>) -> Outcome,
{
    let (state, wake, space, stop) = (pool.state, pool.wake, pool.space, pool.stop);
    let n = jobs.len();
    // First act of the thread, so every later line is covered.
    let _life = WorkerLife { state, wake };
    // `make_worker` is the caller's closure and may panic. Catching it here
    // keeps the panic from propagating out of the scope — which would turn a
    // per-worker fault into a process-level one — and lets the surviving
    // workers claim the whole job list. If none survive, `_life` above is
    // what stops the collector waiting for them.
    let mut ctx = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(make_worker)) {
        Ok(c) => c,
        Err(_) => return,
    };
    loop {
        // Backpressure between jobs. Waiting *before* claiming a job means
        // this worker holds nothing at all — no buffer, no slot, no job.
        {
            let mut st = state.lock().unwrap();
            while !stop.load(Ordering::Acquire) && st.held + st.batch_bytes > pool.budget {
                st = space.wait(st).unwrap();
            }
        }

        // After backpressure, so a worker waiting for output to drain does
        // not also hold a place someone else could use. Held until the job is
        // deposited, at the end of this pass.
        let _permit = match gate {
            Some(g) => match admit(&g.open, inside, stop, next, n) {
                Some(p) => Some(p),
                None => return,
            },
            None => None,
        };

        // Checked before claiming work, so a write failure or a closed pipe
        // starts no further archive.
        if stop.load(Ordering::Acquire) {
            return;
        }
        // Claimed in emission order: the counter indexes `order`, not the job
        // list. The backpressure's deadlock argument rests on this.
        let rank = next.fetch_add(1, Ordering::Relaxed);
        if rank >= n {
            return;
        }
        let pos = order[rank];
        let job = &jobs[pos];

        let mut guard = Completion { state, wake, space, job, pos, sorted: pool.sorted, armed: true };
        let _busy = Busy::new(pool.busy);
        ctl.claimed.fetch_add(1, Ordering::AcqRel);

        let buf = {
            let mut st = state.lock().unwrap();
            st.pool.pop()
        }
        .unwrap_or_else(|| (pool.make_buffer)());

        ctl.lead.store(false, Ordering::Relaxed);
        let mut out = Output::new(buf, pool.segment, job.index, &ctl, Via::Pool { pool, rank, pos });
        let outcome = run_job(job, &mut ctx, &mut out, work);
        let (last, ..) = out.into_last();
        let len = last.len();

        {
            let mut st = state.lock().unwrap();
            let slot = &mut st.slots[pos];
            slot.last = Some((last, outcome));
            slot.bytes += len;
            st.held += len;
            if !pool.sorted {
                st.ready.push_back(pos);
            }
            st.finished += 1;
            guard.armed = false;
        }
        wake.notify_all();
    }
}

/// [`run`], with `before_spawn(i)` called just before worker `i` is spawned.
///
/// A fault-injection point and nothing else: an error from it stands in for
/// the OS refusing a thread, which no test can provoke for real. `run` passes
/// a no-op, which compiles away.
#[allow(clippy::too_many_arguments)]
fn run_with_spawn_hook<C, F, E>(
    jobs: Vec<Job>,
    cfg: Config,
    gate: Option<&Gate>,
    make_buffer: impl Fn() -> termcolor::Buffer + Send + Sync,
    make_worker: impl Fn() -> C + Send + Sync,
    work: F,
    mut emit: E,
    before_spawn: impl Fn(usize) -> io::Result<()>,
) -> io::Result<()>
where
    F: Fn(&Job, &mut C, &mut Output<'_>) -> Outcome + Send + Sync,
    E: FnMut(&Done) -> io::Result<()>,
{
    let Config { workers, sorted, spill_bytes } = cfg;
    let n = jobs.len();
    if n == 0 {
        return Ok(());
    }
    // A budget below a segment is a test's: the segment shrinks to it, so the
    // waits can be exercised without megabytes of output.
    let segment = spill_bytes.clamp(1, SEGMENT);

    if workers <= 1 {
        return run_sequential(&jobs, segment, &make_buffer, &make_worker, &work, &mut emit);
    }

    // Emission order. Sorting by `Job::index` rather than by slot position
    // means a caller that hands us indices which are not `0..n` still gets the
    // order it asked for. Stable, so equal indices keep argument order. For
    // `main`, where index == position, this is the identity. It is also the
    // order jobs are *claimed* in: see the module docs.
    let mut order: Vec<usize> = (0..n).collect();
    if sorted {
        order.sort_by_key(|&p| jobs[p].index);
    }

    let live = workers.min(n);

    let next = AtomicUsize::new(0);
    let stop = Arc::new(AtomicBool::new(false));
    let busy = gate.map(|g| Arc::clone(&g.busy)).unwrap_or_default();
    let starved = Arc::new(AtomicUsize::new(0));
    let claimed = Arc::new(AtomicUsize::new(0));
    let fd_turn = Arc::new(Mutex::new(()));
    // Workers currently holding a place at the gate. Unused without one.
    let inside = AtomicUsize::new(0);
    let state = Mutex::new(Shared {
        slots: (0..n).map(|_| Slot::default()).collect(),
        ready: VecDeque::new(),
        pool: Vec::new(),
        held: 0,
        batch_bytes: 0,
        finished: 0,
        // Seeded before a single thread is spawned, so it only ever counts
        // down: the collector cannot mistake "not started yet" for "all gone".
        live_workers: live,
        head: 0,
        streamer: None,
    });
    let wake = Condvar::new();
    // Workers wait here for the collector to free output bytes, or for their
    // turn. A second condvar on the *same* mutex, so a worker's check-and-wait
    // is atomic against the collector's change and no wakeup can be missed.
    let space = Condvar::new();
    let pool = Pool {
        state: &state,
        wake: &wake,
        space: &space,
        stop: &stop,
        busy: &busy,
        make_buffer: &make_buffer,
        order: &order,
        sorted,
        segment,
        budget: spill_bytes,
        ahead: AHEAD * segment,
    };

    let mut write_err: Option<io::Error> = None;

    let started = std::thread::scope(|scope| {
        // Armed before a single worker is spawned, so that leaving this scope
        // early — a panic out of the spawn loop, or out of `emit` — still
        // releases workers parked on backpressure instead of leaving
        // `thread::scope` to join them forever.
        let _teardown = Teardown { state: &state, space: &space, wake: &wake, stop: &stop };

        let mut started = 0;
        for i in 0..live {
            let ctl = Control {
                stop: Arc::clone(&stop),
                busy: Arc::clone(&busy),
                starved: Arc::clone(&starved),
                claimed: Arc::clone(&claimed),
                fd_turn: Arc::clone(&fd_turn),
                lead: Arc::default(),
            };
            let (pool, jobs, order, next, inside) = (&pool, &jobs[..], &order[..], &next, &inside);
            let (make_worker, work) = (&make_worker, &work);
            // `Builder`, not `scope.spawn`, which panics when the OS refuses a
            // thread — `-j 4300` against macOS's 4096, or a pids cgroup — and
            // a panic is an abort in the release build.
            let spawned = before_spawn(i).and_then(|()| {
                std::thread::Builder::new()
                    .name("trg-worker".into())
                    .spawn_scoped(scope, move || {
                        worker(pool, jobs, order, next, gate, inside, ctl, make_worker, work)
                    })
                    .map(|_| ())
            });
            if let Err(e) = spawned {
                // The workers that never started are no longer expected: the
                // collector must not wait for them to finish anything.
                if let Ok(mut st) = state.lock() {
                    st.live_workers -= live - started;
                }
                wake.notify_all();
                crate::escape::warn(&format!(
                    "could not start worker thread {} of {live} ({e}); searching with {}",
                    i + 1,
                    started.max(1)
                ));
                break;
            }
            started += 1;
        }
        // Not one thread: nothing has been claimed, and the sequential path
        // below needs none.
        if started == 0 {
            return 0;
        }

        // The collector. Runs on the calling thread so `emit` needs to be
        // neither `Send` nor `Sync`, and so ordering decisions live in exactly
        // one place.

        let mut batch: Vec<(usize, Done)> = Vec::new();
        let mut taken = 0usize;

        'collect: loop {
            {
                let mut st = state.lock().unwrap();
                loop {
                    taken += take_ready(&mut st, &jobs, &order, sorted, &mut batch);
                    if !batch.is_empty() || taken >= n {
                        break;
                    }
                    // Not a spurious-wakeup hazard: `finished == n` implies
                    // every job's last piece is in its slot, so the pass above
                    // would have taken them all. Reaching here means work is
                    // still outstanding and some worker will notify — the
                    // `Completion` guard makes that true even for a worker
                    // that unwinds.
                    debug_assert!(st.finished < n, "collector stalled with all work finished");
                    if st.finished >= n {
                        break;
                    }
                    // Jobs outstanding and not one worker left to claim them.
                    // Waiting here is the hang; instead, take whatever landed
                    // and report the rest as unread, which `main` turns into a
                    // 2. Never a silent short read.
                    if st.live_workers == 0 {
                        let stuck = st.streamer.take();
                        for p in stuck.into_iter().chain(order.iter().copied()) {
                            if st.slots[p].taken {
                                continue;
                            }
                            if !take(&mut st, p, jobs[p].index, &mut batch) {
                                let slot = &mut st.slots[p];
                                slot.taken = true;
                                // `make_buffer` under the lock is safe *here*
                                // and nowhere else: there are no workers left
                                // to stall. The buffer stays empty, so it
                                // costs `recycle` nothing.
                                batch.push((
                                    p,
                                    Done {
                                        index: jobs[p].index,
                                        buffer: make_buffer(),
                                        outcome: unclaimed(&jobs[p]),
                                        part: false,
                                        continued: slot.begun,
                                        leads_with_line: false,
                                    },
                                ));
                            }
                            taken += 1;
                        }
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
            // Pieces were taken and the head may have moved: whoever waits for
            // room in its queue, or for its turn, should look again.
            space.notify_all();

            // Emit outside the lock: stdout may block, workers must not.
            let mut pieces = batch.drain(..);
            while let Some((pos, d)) = pieces.next() {
                let r = emit(&d);
                let (index, part) = (d.index, d.part);
                recycle(&state, &space, d.buffer, segment);
                if let Err(e) = r {
                    // Under the lock, so no worker can check `stop` and then
                    // park without seeing it.
                    match state.lock() {
                        Ok(_st) => stop.store(true, Ordering::Release),
                        Err(_) => stop.store(true, Ordering::Release),
                    }
                    space.notify_all();
                    write_err = Some(e);
                    // The write that failed was part of a job still in
                    // flight. Its verdict goes to `emit` all the same, with
                    // nothing to write: what it found unreadable before the
                    // pipe closed is still true, and `-j1` reports it. It is
                    // either later in this batch, or on its way — the job
                    // gives up at its next read now that `stop` is set.
                    if part {
                        let last = pieces
                            .find(|(q, d)| *q == pos && !d.part)
                            .map(|(_, d)| d)
                            .or_else(|| await_last(&state, &wake, pos, index));
                        if let Some(mut d) = last {
                            d.buffer.clear();
                            let _ = emit(&d);
                        }
                    }
                    break 'collect;
                }
            }
            drop(pieces);
            if taken >= n {
                break;
            }
        }

        // Anything still parked is dropped on the way out of the scope; only
        // reachable on the early-stop path, where it is output nobody wants.
        // Waking the blocked producers is `Teardown`'s job, on this path and on
        // the unwinding one alike.
        started
    });

    if started == 0 {
        return run_sequential(&jobs, segment, &make_buffer, &make_worker, &work, &mut emit);
    }

    match write_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Take what the collector may emit now, in the order it may emit it, and
/// return how many jobs that finishes.
fn take_ready(
    st: &mut Shared,
    jobs: &[Job],
    order: &[usize],
    sorted: bool,
    batch: &mut Vec<(usize, Done)>,
) -> usize {
    let mut finished = 0;
    if sorted {
        // The head's segments as it seals them, then its last piece, then the
        // next job's: everything from `head` onwards that has landed.
        while st.head < order.len() {
            let p = order[st.head];
            if !take(st, p, jobs[p].index, batch) {
                break;
            }
            st.head += 1;
            finished += 1;
        }
        return finished;
    }
    // The job that holds the turn first, and nothing else until its last
    // piece: its block must stay unbroken.
    if let Some(s) = st.streamer
        && take(st, s, jobs[s].index, batch)
    {
        st.streamer = None;
        finished += 1;
    }
    if st.streamer.is_none() {
        while let Some(p) = st.ready.pop_front() {
            if !st.slots[p].taken && take(st, p, jobs[p].index, batch) {
                finished += 1;
            }
        }
    }
    finished
}

/// Move every piece job `p` has ready into the batch. `true` once its last
/// piece is among them.
fn take(st: &mut Shared, p: usize, index: usize, batch: &mut Vec<(usize, Done)>) -> bool {
    let slot = &mut st.slots[p];
    let mut moved = 0;
    while let Some(buffer) = slot.parts.pop_front() {
        moved += buffer.len();
        batch.push((
            p,
            Done {
                index,
                buffer,
                outcome: Outcome::default(),
                part: true,
                continued: slot.begun,
                leads_with_line: !slot.begun && slot.lead,
            },
        ));
        slot.begun = true;
    }
    let finished = match slot.last.take() {
        Some((buffer, outcome)) => {
            moved += buffer.len();
            batch.push((
                p,
                Done {
                    index,
                    buffer,
                    leads_with_line: !slot.begun && outcome.leads_with_line,
                    outcome,
                    part: false,
                    continued: slot.begun,
                },
            ));
            slot.begun = true;
            slot.taken = true;
            true
        }
        None => false,
    };
    // Note this does not relieve backpressure: the bytes move from `held` to
    // `batch_bytes`, and only `recycle` frees them.
    slot.bytes -= moved;
    st.held -= moved;
    st.batch_bytes += moved;
    finished
}

/// Wait for job `pos` to finish and return its last piece. Only after `stop`:
/// the job gives up at its next read. `None` if no worker is left to finish it.
fn await_last(state: &Mutex<Shared>, wake: &Condvar, pos: usize, index: usize) -> Option<Done> {
    let mut st = state.lock().ok()?;
    loop {
        if let Some((buffer, outcome)) = st.slots[pos].last.take() {
            st.slots[pos].taken = true;
            return Some(Done { index, buffer, outcome, part: false, continued: true, leads_with_line: false });
        }
        if st.live_workers == 0 {
            return None;
        }
        st = wake.wait(st).ok()?;
    }
}

/// The outcome for a job no worker ever claimed. An `errors` entry is the only
/// partial-run signal `main` reads, so this is what makes the stalled run a 2
/// rather than a confident "no matches, everything read".
fn unclaimed(job: &Job) -> Outcome {
    let mut o = Outcome::default();
    o.errors.push(format!(
        "{}: every worker exited before this archive was searched; not read",
        crate::source::display(&job.path)
    ));
    o
}

/// Return a buffer to the pool so its allocation is reused, unless it grew past
/// a segment — one enormous line should not stay reserved for the rest of the
/// run. `termcolor::Buffer` exposes no `capacity`, so the length it reached
/// stands in for it.
///
/// This is the only place output bytes are actually freed, so it is also where
/// backpressure is released. The pool itself needs no size cap: the number of
/// buffers in existence is bounded by the backpressure invariant, and capping
/// it would only throw away the allocations the pool exists to keep.
fn recycle(state: &Mutex<Shared>, space: &Condvar, mut buffer: termcolor::Buffer, segment: usize) {
    let len = buffer.len();
    let oversized = len > segment;
    buffer.clear();
    if let Ok(mut st) = state.lock() {
        st.batch_bytes = st.batch_bytes.saturating_sub(len);
        if !oversized {
            st.pool.push(buffer);
        }
    }
    space.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Duration;

    const PATIENCE: Duration = Duration::from_secs(10);

    fn jobs(n: usize) -> Vec<Job> {
        (0..n).map(|i| Job { index: i, path: std::path::PathBuf::from(format!("j{i}")) }).collect()
    }

    /// Run on its own thread, so a regression fails after `PATIENCE` instead
    /// of hanging the whole suite.
    fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(PATIENCE).expect("the run hung")
    }

    /// The OS refusing a worker thread used to be a panic out of
    /// `scope.spawn`, and so an abort in the release build (`-j 4300` on
    /// macOS). It is now a warning: the workers that did start carry the
    /// whole list, in order. `spill_bytes` is tiny so that workers park on
    /// backpressure while the spawn fails — deadlock #4 was a parked worker
    /// nobody released on that path.
    #[test]
    fn a_refused_thread_leaves_the_run_to_the_workers_that_started() {
        let seen = within(|| {
            let seen = Mutex::new(Vec::new());
            let r = run_with_spawn_hook(
                jobs(12),
                Config { workers: 4, sorted: true, spill_bytes: 8 },
                None,
                termcolor::Buffer::no_color,
                || (),
                |_job, _c: &mut (), b| {
                    write!(b, "{}", "y".repeat(100)).unwrap();
                    Outcome::default()
                },
                |d: &Done| {
                    if !d.part {
                        seen.lock().unwrap().push((d.index, d.outcome.errors.clone()));
                    }
                    Ok(())
                },
                |i| if i == 2 { Err(io::Error::other("injected: no more threads")) } else { Ok(()) },
            );
            assert!(r.is_ok());
            seen.into_inner().unwrap()
        });
        assert_eq!(seen.iter().map(|(i, _)| *i).collect::<Vec<_>>(), (0..12).collect::<Vec<_>>());
        assert!(seen.iter().all(|(_, e)| e.is_empty()), "nothing went unread: {seen:?}");
    }

    /// Not one thread: the run is done on the calling thread instead.
    #[test]
    fn with_no_thread_at_all_the_run_is_sequential() {
        let seen = within(|| {
            let seen = Mutex::new(Vec::new());
            let r = run_with_spawn_hook(
                jobs(5),
                Config { workers: 4, sorted: true, spill_bytes: 1 << 20 },
                None,
                termcolor::Buffer::no_color,
                || (),
                |job, _c: &mut (), b| {
                    writeln!(b, "job {}", job.index).unwrap();
                    Outcome::default()
                },
                |d: &Done| {
                    seen.lock().unwrap().push(String::from_utf8(d.buffer.as_slice().to_vec()).unwrap());
                    Ok(())
                },
                |_| Err(io::Error::other("injected: no threads")),
            );
            assert!(r.is_ok());
            seen.into_inner().unwrap()
        });
        assert_eq!(seen, (0..5).map(|i| format!("job {i}\n")).collect::<Vec<_>>());
    }

    fn emfile() -> anyhow::Error {
        io::Error::from_raw_os_error(libc::EMFILE).into()
    }

    /// Waiting for a descriptor is worth it while someone else is at work,
    /// and only then.
    #[test]
    fn an_open_out_of_descriptors_is_retried_while_others_are_at_work() {
        let ctl = Control::detached();
        ctl.busy.store(2, Ordering::Relaxed);
        let mut calls = 0;
        let r = ctl.retry_for_fds(|| {
            calls += 1;
            if calls < 4 { Err(emfile()) } else { Ok(calls) }
        });
        assert_eq!(r.unwrap(), 4);
        assert_eq!(ctl.starved.load(Ordering::Relaxed), 0);

        // Alone, or with everyone else waiting too: the error stands, after
        // one more try with nobody at work from its start to its end.
        let alone = Control::alone();
        let mut calls = 0;
        let r: anyhow::Result<()> = alone.retry_for_fds(|| {
            calls += 1;
            Err(emfile())
        });
        assert!(r.is_err());
        assert_eq!(calls, 2);

        // Any other error is not waited on.
        let mut calls = 0;
        let r: anyhow::Result<()> = ctl.retry_for_fds(|| {
            calls += 1;
            Err(io::Error::from_raw_os_error(libc::ENOENT).into())
        });
        assert!(r.is_err());
        assert_eq!(calls, 1);
    }
}
