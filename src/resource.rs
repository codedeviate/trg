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

/// Lower our own scheduling priority. Unprivileged processes may always lower
/// (never raise) their own, so this needs no capabilities and cannot fail in a
/// way worth reporting.
pub fn set_priority(nice: Option<i32>) {
    if let Some(n) = nice {
        unsafe {
            libc::setpriority(libc::PRIO_PROCESS, 0, n);
        }
    }
}

/// Put our reads in the idle I/O class so they queue behind Apache's.
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
/// Callers must pass `drop_cache: false` for plain files — see [`finish`].
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
/// Only ever called with `drop_cache: true` for archives, never for plain
/// files: Apache holds live logs open for append, and evicting those pages
/// would harm the very process this feature exists to protect. Compressed and
/// tar inputs are cold and ours to drop.
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

/// One-minute load average, Linux only. Deliberately read once at startup —
/// no feedback loop.
pub fn load_average_1m() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/loadavg").ok()?;
    s.split_whitespace().next()?.parse().ok()
}

/// Clamp the concurrency to 1 when the box is already busier than the operator
/// said they would tolerate. No source of load (anything but Linux) means no
/// clamping, never a guess.
pub fn clamp_jobs(jobs: usize, limit: Option<f64>) -> usize {
    match (limit, load_average_1m()) {
        (Some(max), Some(now)) if now > max => 1,
        _ => jobs,
    }
}
