//! Staying out of the way on a loaded production server.
//!
//! Priority, not thread starvation, is the primary lever: a nice'd worker uses
//! idle capacity and is preempted the instant Apache wants the core. Measured
//! separately on 610 MB of real archives: `FADV_DONTNEED` takes page-cache
//! residency from 610 MB to 0 MB at no cost in wall time or CPU (33.68 s vs
//! 34.14 s, within noise), where `zgrep -a`, `rg -za` and `rg -za -j1` all
//! leave the full 610 MB resident and evict whatever Apache and MariaDB had
//! warm.
//!
//! **The platform asymmetry is real and is not papered over.** `FADV_DONTNEED`
//! evicts pages *after* reading; macOS has no `posix_fadvise` at all and its
//! nearest equivalent, `fcntl(fd, F_NOCACHE, 1)`, prevents caching *before*
//! reading. So there are two hooks — [`prepare`] at open and [`finish`] after
//! the archive has been read — and each platform does its real work in a
//! different one:
//!
//! | | Linux | macOS |
//! |---|---|---|
//! | [`prepare`] | `FADV_SEQUENTIAL` (readahead hint) | `F_NOCACHE` (the actual work) |
//! | [`finish`]  | `FADV_DONTNEED` (the actual work)  | no-op |

use std::fs::File;
use std::sync::atomic::Ordering;

/// Lower our own scheduling priority, and only ever lower it.
///
/// `setpriority` sets an absolute niceness, so run as root from a job already
/// at nice 19, the default 10 *raised* trg above the job that started it. A
/// niceness at or past the one asked for is left where it is.
///
/// A negative value is the exception, being a request to raise it that
/// someone typed: it is tried, and without the privilege it fails. That is
/// said once, on stderr, and changes nothing else: every byte is read all
/// the same, so the exit code is not its to change.
pub fn set_priority(nice: Option<i32>) {
    let Some(n) = nice else { return };
    if current_niceness().is_some_and(|now| now == n || (n >= 0 && now > n)) {
        return;
    }
    if unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, n) } != 0 {
        let e = std::io::Error::last_os_error();
        crate::escape::warn(&format!("--nice {n}: priority left as it was: {e}"));
    }
}

/// Our niceness, `None` when it cannot be read. -1 is both a niceness and
/// `getpriority`'s error value, told apart by `errno`, which therefore has to
/// be cleared first.
fn current_niceness() -> Option<i32> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let errno = unsafe { libc::__errno_location() };
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    let errno = unsafe { libc::__error() };
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    unsafe {
        *errno = 0;
        let now = libc::getpriority(libc::PRIO_PROCESS, 0);
        (now != -1 || *errno == 0).then_some(now)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos", target_os = "ios", target_os = "freebsd")))]
    None
}

/// Put our reads in the idle I/O class so they queue behind Apache's.
///
/// Call it before any thread is started, as [`set_priority`]: on Linux both
/// act on the calling thread alone (`who` 0 is this thread, and niceness is
/// per-thread there too), and a thread inherits them from the one that
/// starts it. Called after the workers exist, it would leave them all at the
/// priority trg was launched with. Its result is not looked at: where the
/// class cannot be set the search is no less correct, only less polite.
///
/// `ioprio_set` has no libc wrapper on Linux, so it goes through `syscall(2)`
/// with `SYS_ioprio_set` (30 on aarch64, 251 on x86_64 — `libc` picks the
/// right one per arch). Arguments are widened to `c_long` explicitly: they
/// travel through a variadic, and letting a `c_int` be promoted leaves the
/// upper half of the register unspecified.
#[cfg(target_os = "linux")]
pub fn set_io_idle() {
    const IOPRIO_WHO_PROCESS: libc::c_long = 1;
    const IOPRIO_CLASS_IDLE: libc::c_long = 3;
    const IOPRIO_CLASS_SHIFT: libc::c_long = 13;
    unsafe {
        libc::syscall(
            libc::SYS_ioprio_set,
            IOPRIO_WHO_PROCESS,
            0 as libc::c_long,
            IOPRIO_CLASS_IDLE << IOPRIO_CLASS_SHIFT,
        );
    }
}

/// macOS has no I/O scheduling classes; `IOPOL_THROTTLE` on the process-wide
/// disk I/O policy is the closest analogue.
#[cfg(target_os = "macos")]
pub fn set_io_idle() {
    const IOPOL_TYPE_DISK: libc::c_int = 0;
    const IOPOL_SCOPE_PROCESS: libc::c_int = 0;
    const IOPOL_THROTTLE: libc::c_int = 3;
    unsafe extern "C" {
        fn setiopolicy_np(
            iotype: libc::c_int,
            scope: libc::c_int,
            policy: libc::c_int,
        ) -> libc::c_int;
    }
    unsafe {
        setiopolicy_np(IOPOL_TYPE_DISK, IOPOL_SCOPE_PROCESS, IOPOL_THROTTLE);
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn set_io_idle() {}

/// Called immediately after opening an archive, before it is read.
///
/// Linux hints sequential readahead; macOS must set `F_NOCACHE` *here* because
/// it prevents caching rather than evicting afterwards. `drop_cache` is
/// therefore honoured on macOS at this point and on Linux only in [`finish`].
///
/// Callers must pass `drop_cache: false` for plain files — see [`finish`] —
/// except one found to be binary, which no live log is.
#[cfg(target_os = "linux")]
pub fn prepare(f: &File, _drop_cache: bool) {
    use std::os::unix::io::AsRawFd;
    unsafe {
        libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_SEQUENTIAL);
    }
}

#[cfg(target_os = "macos")]
pub fn prepare(f: &File, drop_cache: bool) {
    use std::os::unix::io::AsRawFd;
    if drop_cache {
        unsafe {
            libc::fcntl(f.as_raw_fd(), libc::F_NOCACHE, 1);
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn prepare(_f: &File, _drop_cache: bool) {}

/// Called after an archive has been fully read.
///
/// Only ever called with `drop_cache: true` for archives and binary plain
/// files, never for a plain text file: Apache holds live logs open for
/// append, and evicting those pages would harm the very process this feature
/// exists to protect. Compressed and tar inputs are cold and ours to drop,
/// and so is a binary file: see `BinaryCache` in `archive.rs`.
///
/// `FADV_DONTNEED` drops clean pages only. An archive written moments ago,
/// logrotate's newest, can still be dirty, and stays cached until the kernel
/// writes it back. trg does not sync it: forcing writes on a busy server is
/// worse than a few cached pages.
#[cfg(target_os = "linux")]
pub fn finish(f: &File, drop_cache: bool) {
    use std::os::unix::io::AsRawFd;
    if drop_cache {
        unsafe {
            libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub fn finish(_f: &File, _drop_cache: bool) {
    // macOS did its work in `prepare` via F_NOCACHE; nothing to undo.
}

/// One-minute load average, from `getloadavg(3)`: `/proc/loadavg` on Linux,
/// the kernel's own figure on macOS. `None` where there is none.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn load_average_1m() -> Option<f64> {
    let mut avg = [0f64; 1];
    let got = unsafe { libc::getloadavg(avg.as_mut_ptr(), 1) };
    (got == 1).then_some(avg[0])
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn load_average_1m() -> Option<f64> {
    None
}

/// How often the gate is reconsidered. Linux recomputes the load average every
/// 5 s, so checking more often only reads the same number again.
const LOAD_CHECK: std::time::Duration = std::time::Duration::from_secs(5);

/// The gate before any worker has started. All of the load is someone else's
/// at this point, so it is compared as it stands.
pub fn starting_gate(jobs: usize, limit: f64, load: f64) -> usize {
    if load > limit { 1 } else { jobs.max(1) }
}

/// The gate after one more look at the load.
///
/// The load average counts trg's own workers too, so up to `gate` of it is
/// ours and comes off before the comparison. Without that, `-j8 --load-limit
/// 4` closes the gate on itself on an idle box. Over the limit, it drops
/// straight to one job: the point of the flag is to get out of Apache's way,
/// and by the time a 1-minute average shows a spike, the spike has been going
/// for a while. Under it, it opens one worker per check, because the average
/// lags in the other direction too, and snapping wide open after a spike
/// walks straight back into it.
pub fn next_gate(gate: usize, jobs: usize, load: f64, limit: f64) -> usize {
    let others = load - gate as f64;
    if others > limit { 1 } else { (gate + 1).min(jobs).max(1) }
}

/// Start adapting the concurrency to the load, or `None` when there is nothing
/// to adapt: no `--load-limit`, a single job, or no load average to read.
///
/// The returned gate is what [`crate::sched::run_gated`] admits. A detached
/// thread moves it every [`LOAD_CHECK`] and stops once nobody else holds the
/// gate, so it costs one sleeping thread for the length of the run.
pub fn load_gate(jobs: usize, limit: Option<f64>) -> Option<std::sync::Arc<crate::sched::Gate>> {
    let limit = limit?;
    if jobs <= 1 {
        return None;
    }
    let gate = std::sync::Arc::new(crate::sched::Gate::new(starting_gate(jobs, limit, load_average_1m()?)));
    let held = std::sync::Arc::downgrade(&gate);
    // No thread means no adapting, not no search and not no limit: the gate
    // stays where it started, which is what the old one-off check did.
    let _ = std::thread::Builder::new()
        .name("trg-load".into())
        .spawn(move || {
            loop {
                std::thread::sleep(LOAD_CHECK);
                let Some(gate) = held.upgrade() else { return };
                if let Some(load) = load_average_1m() {
                    let now = gate.open.load(Ordering::Relaxed);
                    gate.open.store(next_gate(now, jobs, load, limit), Ordering::Relaxed);
                }
            }
        });
    Some(gate)
}
