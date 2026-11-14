//! The scheduler and its output path, hunted: memory that grew with the match
//! volume, sorted output that left the order, a pipe closed by `head` that
//! was written to for seconds more, a load gate that never closed, and the
//! smaller things found on the way.

mod helpers;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use trg::sched::{Config, Done, Job};

const TRG: &str = env!("CARGO_BIN_EXE_trg");

fn jobs(n: usize) -> Vec<Job> {
    (0..n).map(|i| Job { index: i, path: PathBuf::from(format!("j{i}")) }).collect()
}

/// See the same helper in `ordering.rs`: a deadlock must fail the test, not
/// wedge the suite.
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

/// `n` copies of one archive whose single member is `mb` MB of lines that
/// all match `ERROR`. Each copy prints more than its input: every line gets
/// the archive's path in front.
fn heavy(n: usize, mb: usize) -> (tempfile::TempDir, Vec<PathBuf>) {
    let line = b"the quick brown fox jumps over the lazy dog GET /index.html 200 ERROR\n";
    let body: Vec<u8> = line.iter().copied().cycle().take(mb << 20).collect();
    let made = helpers::tgz("heavy.tgz", &[("logs/huge.log", &body)]);
    let d = tempfile::tempdir().unwrap();
    let paths = (0..n)
        .map(|i| {
            let p = d.path().join(format!("a{i}.tgz"));
            std::fs::copy(&made.path, &p).unwrap();
            p
        })
        .collect();
    (d, paths)
}

fn stdout_of(args: &[&str], paths: &[PathBuf]) -> (Vec<u8>, Option<i32>, String) {
    let out = Command::new(TRG).args(args).args(paths).output().unwrap();
    (out.stdout, out.status.code(), String::from_utf8_lossy(&out.stderr).into_owned())
}

// ---- 1. memory does not grow with the match volume ------------------------

/// Run trg with its output thrown away and return its peak resident set in
/// MB, from the kernel's own account of the child, and its exit code.
///
/// The child is reaped by `wait4` below, which is how its resource usage is
/// had; `Child::wait` would reap it first and leave nothing to ask.
#[cfg(target_os = "macos")]
#[allow(clippy::zombie_processes)]
fn peak_rss_mb(args: &[&str], paths: &[PathBuf]) -> (f64, Option<i32>) {
    let child = Command::new(TRG)
        .args(args)
        .args(paths)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut status = 0;
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    let got = unsafe { libc::wait4(child.id() as libc::pid_t, &mut status, 0, &mut ru) };
    assert_eq!(got, child.id() as libc::pid_t, "wait4 failed");
    let code = libc::WIFEXITED(status).then(|| libc::WEXITSTATUS(status));
    (ru.ru_maxrss as f64 / 1_048_576.0, code)
}

/// Not `wait4`'s `ru_maxrss` here: Linux carries the peak of the process
/// that forked into the child across its exec, so the figure is this test
/// binary's own peak, some 700 MB with the fixtures it builds. The child's
/// address space keeps its own high-water mark, `VmHWM`, which only grows,
/// so the last reading before it exits is its peak to within a poll.
#[cfg(all(unix, not(target_os = "macos")))]
fn peak_rss_mb(args: &[&str], paths: &[PathBuf]) -> (f64, Option<i32>) {
    let mut child = Command::new(TRG)
        .args(args)
        .args(paths)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let status_file = format!("/proc/{}/status", child.id());
    let mut peak_kb = 0u64;
    loop {
        let hwm = std::fs::read_to_string(&status_file).ok().and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("VmHWM:"))
                .and_then(|v| v.split_whitespace().next())
                .and_then(|v| v.parse::<u64>().ok())
        });
        peak_kb = peak_kb.max(hwm.unwrap_or(0));
        if let Some(st) = child.try_wait().unwrap() {
            return (peak_kb as f64 / 1024.0, st.code());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// One archive's whole output used to sit in memory until the archive was
/// done: 1.6 GB of resident set for a 4 MB `.tgz` in which every line
/// matched, at `-j1`. Here 48 MB of lines print as some 100 MB per archive,
/// which is what the old `-j1` held at once.
#[cfg(unix)]
#[test]
fn heavy_output_is_written_in_flat_memory_at_j1() {
    let (_d, paths) = heavy(2, 48);
    let (mb, code) = peak_rss_mb(&["-j1", "-N", "ERROR"], &paths);
    assert_eq!(code, Some(0));
    assert!(mb < 40.0, "peak resident set {mb:.0} MB for output that should stream");
}

/// With workers, the output waiting its turn is bounded by the budget
/// (64 MB), not by how much four archives print: some 400 MB here, all of
/// which the old scheduler could hold.
#[cfg(unix)]
#[test]
fn heavy_output_is_written_in_bounded_memory_at_j4() {
    let (_d, paths) = heavy(4, 48);
    let (mb, code) = peak_rss_mb(&["-j4", "-N", "ERROR"], &paths);
    assert_eq!(code, Some(0));
    assert!(mb < 160.0, "peak resident set {mb:.0} MB under a 64 MB budget");
    let (mb, code) = peak_rss_mb(&["-j4", "--no-sort", "-N", "ERROR"], &paths);
    assert_eq!(code, Some(0));
    assert!(mb < 160.0, "peak resident set {mb:.0} MB under a 64 MB budget, unsorted");
}

/// `-T` buffered the listing the same way: a gigabyte for an archive of
/// many long names.
#[cfg(unix)]
#[test]
fn a_long_listing_is_written_in_flat_memory() {
    // 60,000 members with names of 900 bytes: 54 MB of listing.
    let dir = "d".repeat(90);
    let long = std::iter::repeat_n(dir.as_str(), 9).collect::<Vec<_>>().join("/");
    let names: Vec<String> = (0..60_000).map(|i| format!("{long}/{i:06}.log")).collect();
    let members: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b"x\n"[..])).collect();
    let f = helpers::tgz("names.tgz", &members);
    let (mb, code) = peak_rss_mb(&["-T"], std::slice::from_ref(&f.path));
    assert_eq!(code, Some(0));
    assert!(mb < 40.0, "peak resident set {mb:.0} MB for a listing that should stream");
}

/// An archive in which every member fails kept every error: 1.9 GB. The
/// first hundred are reported, the rest counted, and it is a 2 all the same.
#[test]
fn an_archive_of_failing_members_reports_a_hundred_errors_and_counts_the_rest() {
    // A gzip member cut in half: an error for each.
    let whole = helpers::gzip_bytes(&vec![b'x'; 4096]);
    let cut = &whole[..whole.len() / 2];
    let names: Vec<String> = (0..350).map(|i| format!("logs/m{i:03}.log.gz")).collect();
    let members: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), cut)).collect();
    let f = helpers::tgz("bad.tgz", &members);
    let (_, code, err) = stdout_of(&["NEEDLE"], std::slice::from_ref(&f.path));
    assert_eq!(code, Some(2), "{err}");
    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(lines.len(), 101, "100 errors and one line for the rest:\n{err}");
    assert!(lines[0].contains("m000.log.gz"), "{}", lines[0]);
    assert!(lines[100].contains("bad.tgz") && lines[100].contains("250 more errors"), "{}", lines[100]);
}

/// A member's name in a diagnostic is one line of reasonable length, however
/// long the archive made it.
#[test]
fn a_very_long_member_name_is_cut_short_in_a_diagnostic() {
    let whole = helpers::gzip_bytes(&vec![b'x'; 4096]);
    let name = format!("{}/x.log.gz", vec!["n".repeat(200); 30].join("/"));
    assert!(name.len() > 6000);
    let f = helpers::tgz("long.tgz", &[(name.as_str(), &whole[..whole.len() / 2])]);
    let (_, code, err) = stdout_of(&["NEEDLE"], std::slice::from_ref(&f.path));
    assert_eq!(code, Some(2), "{err}");
    assert!(err.len() < 2000, "a {}-byte diagnostic for one member", err.len());
    assert!(err.contains('…'), "{err}");
}

// ---- 1b. a closed pipe ends the run ---------------------------------------

/// Start trg, read its first line, close the pipe, and return how long the
/// process took from start to exit, its exit code and its stderr.
fn head_one(args: &[&str], paths: &[PathBuf]) -> (Duration, Option<i32>, String) {
    let started = Instant::now();
    let mut child = Command::new(TRG)
        .args(args)
        .args(paths)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = child.stdout.take().unwrap();
    let mut first = [0u8; 64];
    out.read_exact(&mut first).unwrap();
    drop(out);
    let done = child.wait_with_output().unwrap();
    (started.elapsed(), done.status.code(), String::from_utf8_lossy(&done.stderr).into_owned())
}

/// How long the same search takes when all of its output is read.
fn whole_run(args: &[&str], paths: &[PathBuf]) -> Duration {
    let started = Instant::now();
    let status = Command::new(TRG).args(args).args(paths).stdout(Stdio::null()).status().unwrap();
    assert_eq!(status.code(), Some(0));
    started.elapsed()
}

/// `trg ERROR big.tgz | head -1` went on searching after `head` had gone,
/// because the pipe was only written to, and so only found closed, once an
/// archive was finished. Now the first segment is written while the archive
/// is being searched, and the search ends at the write that fails.
#[test]
fn a_closed_pipe_ends_the_run_at_once_at_j1() {
    let (_d, paths) = heavy(2, 48);
    let whole = whole_run(&["-j1", "-N", "ERROR"], &paths);
    let (took, code, err) = head_one(&["-j1", "-N", "ERROR"], &paths);
    assert_eq!(code, Some(0), "a closed pipe after a match is a 0: {err}");
    assert!(err.is_empty(), "a closed pipe is not worth a diagnostic: {err}");
    // Finishing the first of the two archives, as it used to, is half.
    assert!(took < whole / 4, "took {took:?} after the pipe closed; the whole search takes {whole:?}");
}

/// And with workers: the ones in flight give up at their next read, not at
/// the end of their archives.
#[test]
fn a_closed_pipe_ends_the_run_at_once_at_j4() {
    let (_d, paths) = heavy(4, 48);
    let whole = whole_run(&["-j4", "-N", "ERROR"], &paths);
    for sort in [&["-j4", "-N", "ERROR"][..], &["-j4", "--no-sort", "-N", "ERROR"][..]] {
        let (took, code, err) = head_one(sort, &paths);
        assert_eq!(code, Some(0), "a closed pipe after a match is a 0: {err}");
        assert!(err.is_empty(), "a closed pipe is not worth a diagnostic: {err}");
        assert!(took < whole / 4, "took {took:?} after the pipe closed; the whole search takes {whole:?}");
    }
}

/// What an archive found unreadable before the pipe closed is still true,
/// and still said: a 2, as when the archive is small enough to be written in
/// one piece.
#[test]
fn an_error_found_before_the_pipe_closed_is_still_a_two() {
    let line = b"the quick brown fox jumps over the lazy dog GET /index.html 200 ERROR\n";
    let body: Vec<u8> = line.iter().copied().cycle().take(24 << 20).collect();
    let whole = helpers::gzip_bytes(b"lost\n");
    let f = helpers::tgz("mixed.tgz", &[("logs/cut.log.gz", &whole[..whole.len() / 2]), ("logs/huge.log", &body)]);
    // Copies: the same file named three times is searched once.
    let paths: Vec<PathBuf> = (0..3)
        .map(|i| {
            let p = f.dir.path().join(format!("mixed{i}.tgz"));
            std::fs::copy(&f.path, &p).unwrap();
            p
        })
        .collect();
    for j in ["-j1", "-j3"] {
        let (_, code, err) = head_one(&[j, "-N", "ERROR"], &paths);
        assert_eq!(code, Some(2), "{j}: {err}");
        assert!(err.contains("cut.log.gz"), "{j}: {err}");
        assert!(!err.contains("write error") && !err.contains("stopped"), "{j}: {err}");
    }
}

// ---- 2. sorted output is in order, whatever is waiting --------------------

/// Everything `emit` was handed, in the order it came, as (job, bytes).
fn run_collecting(workers: usize, sorted: bool, spill_bytes: usize, n: usize, lines: usize) -> Vec<(usize, Vec<u8>)> {
    within(60, move || {
        let seen = Mutex::new(Vec::new());
        trg::sched::run(
            jobs(n),
            Config { workers, sorted, spill_bytes },
            termcolor::Buffer::no_color,
            || (),
            |job, _c: &mut (), out| {
                // Uneven, so completion order is not argument order.
                if job.index % 3 == 0 {
                    std::thread::sleep(Duration::from_millis(15));
                }
                for l in 0..lines {
                    writeln!(out, "job {} line {l} {}", job.index, "x".repeat(40)).unwrap();
                }
                trg::archive::Outcome::default()
            },
            |d: &Done| {
                seen.lock().unwrap().push((d.index, d.buffer.as_slice().to_vec()));
                Ok(())
            },
        )
        .unwrap();
        seen.into_inner().unwrap()
    })
}

fn bytes_of(pieces: &[(usize, Vec<u8>)]) -> Vec<u8> {
    pieces.iter().flat_map(|(_, b)| b.iter().copied()).collect()
}

/// The escape valve that flushed parked output out of order fired whenever
/// more than the cap was waiting, the head job still running: `-j8` over 40
/// archives of 30 MB of matches each came out in a different order every
/// other run, with exit 0. The cap is injected here so that what waits
/// exceeds it many times over without gigabytes of output.
#[test]
fn eight_workers_print_what_one_does_byte_for_byte_past_the_cap() {
    // 24 jobs of some 100 KB against a 16 KB cap.
    let one = bytes_of(&run_collecting(1, true, 16 << 10, 24, 2000));
    for _ in 0..5 {
        let eight = run_collecting(8, true, 16 << 10, 24, 2000);
        assert!(eight.len() > 24, "nothing was streamed: the cap was never reached");
        assert!(bytes_of(&eight) == one, "-j8 and -j1 differ");
    }
}

/// `--no-sort` gives up the order between archives and nothing else: one
/// archive's output is one unbroken block, also when it is written in
/// pieces because it outgrew the cap.
#[test]
fn unsorted_output_keeps_each_archive_in_one_block() {
    for _ in 0..5 {
        let pieces = run_collecting(8, false, 16 << 10, 24, 2000);
        let mut blocks: Vec<usize> = pieces.iter().map(|(i, _)| *i).collect();
        blocks.dedup();
        let mut distinct = blocks.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(blocks.len(), distinct.len(), "a job's output came in two blocks: {blocks:?}");
        assert_eq!(distinct, (0..24).collect::<Vec<_>>());
        // And within its block, a job's lines are its own, in order.
        let all = String::from_utf8(bytes_of(&pieces)).unwrap();
        let mut next = [0usize; 24];
        for line in all.lines() {
            let mut w = line.split(' ');
            let (job, l): (usize, usize) = (w.nth(1).unwrap().parse().unwrap(), w.nth(1).unwrap().parse().unwrap());
            assert_eq!(l, next[job], "job {job} out of sequence");
            next[job] += 1;
        }
        assert!(next.iter().all(|&n| n == 2000));
    }
}

/// An order that is not the order of the job list, with fewer workers than
/// it takes to reach the job that goes first by claiming down the list:
/// parked workers waited for a job nobody was left to claim. Jobs are
/// claimed in the order they are emitted.
#[test]
fn the_job_that_goes_first_is_claimed_first_wherever_it_is_listed() {
    let seen = within(30, || {
        let js: Vec<Job> = [3usize, 2, 1, 0]
            .iter()
            .map(|&index| Job { index, path: PathBuf::from(format!("j{index}")) })
            .collect();
        let seen = Mutex::new(Vec::new());
        trg::sched::run(
            js,
            Config { workers: 2, sorted: true, spill_bytes: 64 },
            termcolor::Buffer::no_color,
            || (),
            |job, _c: &mut (), out| {
                for l in 0..200 {
                    writeln!(out, "job {} line {l}", job.index).unwrap();
                }
                trg::archive::Outcome::default()
            },
            |d: &Done| {
                seen.lock().unwrap().push(d.index);
                Ok(())
            },
        )
        .unwrap();
        seen.into_inner().unwrap()
    });
    let mut sorted = seen.clone();
    sorted.sort_unstable();
    assert_eq!(seen, sorted);
}

/// A write that fails on a piece of a job still in flight stops that job,
/// and its verdict is still handed over, once and with nothing to write.
#[test]
fn a_failed_write_mid_job_stops_it_and_still_delivers_its_verdict() {
    for workers in [1, 4] {
        let (r, verdicts, wrote) = within(30, move || {
            let verdicts = Mutex::new(Vec::new());
            let wrote = Mutex::new(Vec::new());
            let r = trg::sched::run(
                jobs(6),
                Config { workers, sorted: true, spill_bytes: 4096 },
                termcolor::Buffer::no_color,
                || (),
                |job, _c: &mut (), out| {
                    let mut o = trg::archive::Outcome::default();
                    o.errors.push(format!("found before the stop in {}", job.index));
                    let mut lines = 0;
                    for l in 0..100_000 {
                        if writeln!(out, "job {} line {l}", job.index).is_err() {
                            break;
                        }
                        lines += 1;
                    }
                    wrote.lock().unwrap().push((job.index, lines));
                    o
                },
                |d: &Done| {
                    if d.part {
                        return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
                    }
                    verdicts.lock().unwrap().push((d.index, d.buffer.len(), d.outcome.errors.clone()));
                    Ok(())
                },
            );
            (r, verdicts.into_inner().unwrap(), wrote.into_inner().unwrap())
        });
        assert_eq!(r.unwrap_err().kind(), std::io::ErrorKind::BrokenPipe, "workers={workers}");
        assert_eq!(
            verdicts,
            vec![(0, 0, vec!["found before the stop in 0".to_string()])],
            "workers={workers}: the stopped job's verdict, and no other"
        );
        assert!(
            wrote.iter().all(|&(_, lines)| lines < 100_000),
            "workers={workers}: a job wrote to its end after the stop: {wrote:?}"
        );
    }
}

// ---- 1c. the `--` between archives, when the archive is written in pieces --

/// The `--` before an archive's first line is decided when its first bytes
/// are written, which for an archive that prints more than a segment is
/// before it has been searched to its end.
#[test]
fn the_separator_between_archives_is_the_same_when_an_archive_streams() {
    // 3 MB of lines: with `-C1` every line prints, in several segments.
    let line = b"an ordinary line with the word lazy in it, long enough to add up\n";
    let body: Vec<u8> = line.iter().copied().cycle().take(3 << 20).collect();
    let big = helpers::tgz("big.tgz", &[("logs/a.log", &body)]);
    let small = helpers::tgz("small.tgz", &[("logs/b.log", b"one lazy line\n")]);
    // A file of its own: the same one named twice is searched once.
    let last = helpers::tgz("last.tgz", &[("logs/c.log", b"another lazy line\n")]);
    // An archive whose output leads with `binary file matches`: no `--`
    // before it, though it goes on to print lines enough to stream.
    let mut bin = b"lazy \0 binary\n".to_vec();
    bin.extend_from_slice(&[0u8; 64]);
    let binary_first = helpers::tgz("binfirst.tgz", &[("logs/0.bin", &bin), ("logs/z.log", &body)]);

    let alone = |p: &Path, crlf: bool| {
        let mut args = vec!["-j1", "-C1"];
        if crlf {
            args.push("--crlf");
        }
        args.push("lazy");
        let (out, code, err) = stdout_of(&args, &[p.to_path_buf()]);
        assert_eq!(code, Some(0), "{err}");
        out
    };
    for crlf in [false, true] {
        let sep: &[u8] = if crlf { b"--\r\n" } else { b"--\n" };
        let mut want = alone(&small.path, crlf);
        want.extend_from_slice(sep);
        want.extend(alone(&big.path, crlf));
        // No `--` here: the next archive leads with `binary file matches`.
        want.extend(alone(&binary_first.path, crlf));
        want.extend_from_slice(sep);
        want.extend(alone(&last.path, crlf));
        let paths = [small.path.clone(), big.path.clone(), binary_first.path.clone(), last.path.clone()];
        for j in ["-j1", "-j4"] {
            let mut args = vec![j, "-C1"];
            if crlf {
                args.push("--crlf");
            }
            args.push("lazy");
            let (got, code, err) = stdout_of(&args, &paths);
            assert_eq!(code, Some(0), "{err}");
            assert!(got == want, "{j} crlf={crlf}: the output differs from the archives' own, joined");
        }
    }
}

/// Colour sequences are bytes of the output like any others: the same at
/// `-j4`, in pieces, as at `-j1`.
#[test]
fn coloured_output_is_the_same_in_pieces() {
    let (_d, paths) = heavy(3, 3);
    let (one, code, _) = stdout_of(&["-j1", "--color", "always", "fox"], &paths);
    assert_eq!(code, Some(0));
    assert!(one.windows(2).any(|w| w == b"\x1b["), "no colour in the output");
    let (four, code, _) = stdout_of(&["-j4", "--color", "always", "fox"], &paths);
    assert_eq!(code, Some(0));
    assert!(one == four, "coloured output differs between -j1 and -j4");
}

/// `--json` ends with a summary of every archive, streamed or not.
#[test]
fn the_json_summary_counts_every_archive_when_they_stream() {
    let (_d, paths) = heavy(3, 3);
    let (out, code, err) = stdout_of(&["-j3", "--json", "ERROR"], &paths);
    assert_eq!(code, Some(0), "{err}");
    let text = String::from_utf8(out).unwrap();
    let matches = text.lines().filter(|l| l.starts_with("{\"type\":\"match\"")).count();
    let summary = text.lines().last().unwrap();
    assert!(summary.contains("\"type\":\"summary\""), "{summary}");
    assert!(summary.contains(&format!("\"matched_lines\":{matches}")), "{matches} matches, but: {summary}");
}

// ---- 3. the load gate -------------------------------------------------------

/// The gate took its own height off the load, as if every place at it were a
/// running worker. `--turbo` on 64 cores over seven archives has a gate of
/// 64, so the others' load was `load - 64` and no load could close it.
#[test]
fn the_gate_closes_on_load_that_is_not_ours() {
    use trg::resource::next_gate;
    // Seven workers busy, a load of 30: 23 of it is someone else's.
    assert_eq!(next_gate(64, 64, 30.0, 4.0, 7.0), 1);
    // Workers waiting their turn to write are no load: all of it is theirs.
    assert_eq!(next_gate(8, 8, 9.0, 4.0, 0.0), 1);
    // And the case it must not break: eight of ours and little else.
    assert_eq!(next_gate(8, 8, 9.0, 4.0, 8.0), 8);
    // Sampled at different moments, ours can exceed the load: that is zero
    // of theirs, not a negative amount.
    assert_eq!(next_gate(3, 8, 2.0, 0.0, 5.0), 4);
}

/// trg's share is averaged as the kernel averages the load: towards the
/// number of busy workers, over about a minute.
#[test]
fn our_share_of_the_load_follows_the_busy_workers_as_the_load_does() {
    use trg::resource::own_load;
    let step = Duration::from_secs(5);
    let mut ours = 0.0;
    ours = own_load(ours, 8, step);
    assert!(ours > 0.5 && ours < 0.8, "one sample of eight busy workers: {ours}");
    for _ in 0..60 {
        ours = own_load(ours, 8, step);
    }
    assert!((ours - 8.0).abs() < 0.1, "five minutes of eight busy workers: {ours}");
    for _ in 0..60 {
        ours = own_load(ours, 0, step);
    }
    assert!(ours < 0.1, "five minutes of none: {ours}");
}

/// The gate reports how many workers are at work, which is what the load
/// thread reads: none before, none after, and no more than the jobs there
/// are, however wide `-j` is.
#[test]
fn the_gate_counts_the_workers_that_are_busy() {
    let peak = within(30, || {
        let gate = trg::sched::Gate::new(64);
        let peak = std::sync::atomic::AtomicUsize::new(0);
        assert_eq!(gate.busy(), 0);
        trg::sched::run_gated(
            jobs(3),
            Config { workers: 64, sorted: true, spill_bytes: 64 << 20 },
            &gate,
            termcolor::Buffer::no_color,
            || (),
            |_job, _c: &mut (), _out| {
                std::thread::sleep(Duration::from_millis(100));
                peak.fetch_max(gate.busy(), std::sync::atomic::Ordering::SeqCst);
                trg::archive::Outcome::default()
            },
            |_d: &Done| Ok(()),
        )
        .unwrap();
        assert_eq!(gate.busy(), 0);
        peak.into_inner()
    });
    assert_eq!(peak, 3, "three archives are three busy workers at -j64");
}

// ---- 6. a low descriptor limit ----------------------------------------------

/// `-j` far above what `ulimit -n` lets be open at once: the workers that
/// could not open their file took the next, failed again, and raced through
/// the list, so nearly every file was reported unread. They wait for a
/// descriptor now, as long as another worker is at work and will free one.
#[cfg(unix)]
#[test]
fn more_workers_than_descriptors_wait_instead_of_failing() {
    let d = tempfile::tempdir().unwrap();
    for i in 0..300 {
        let body = format!("{}NEEDLE in file {i}\n", "padding line\n".repeat(2000));
        std::fs::write(d.path().join(format!("f{i:03}.log")), body).unwrap();
    }
    let script = format!("ulimit -n 32; exec {TRG} -j64 -c NEEDLE {}", d.path().display());
    let out = Command::new("sh").arg("-c").arg(&script).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 300, "{err}");
}

// ---- 8. -T on a whole compressed file ---------------------------------------

/// `-T` on a cut-off `.tgz` is a 2: the walk to its end finds the cut. A
/// compressed file that is not a tar was listed and never read, so the same
/// cut was a 0.
#[test]
fn listing_a_cut_off_compressed_file_is_a_two() {
    let d = tempfile::tempdir().unwrap();
    let whole = helpers::gzip_bytes(&b"a line of log\n".repeat(50_000));
    let (ok, cut) = (d.path().join("ok.log.gz"), d.path().join("cut.log.gz"));
    std::fs::write(&ok, &whole).unwrap();
    std::fs::write(&cut, &whole[..whole.len() / 2]).unwrap();

    let (out, code, err) = stdout_of(&["-T"], std::slice::from_ref(&ok));
    assert_eq!(code, Some(0), "{err}");
    assert!(String::from_utf8_lossy(&out).contains("ok.log.gz"));

    let (out, code, err) = stdout_of(&["-T"], std::slice::from_ref(&cut));
    assert_eq!(code, Some(2), "a cut-off file listed as whole");
    assert!(err.contains("cut.log.gz"), "{err}");
    assert!(String::from_utf8_lossy(&out).contains("cut.log.gz"), "it is still listed");
}
