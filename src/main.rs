use termcolor::{BufferWriter, ColorChoice};
// `warn`, never `eprintln!`, which panics when stderr cannot be written.
use trg::{cli::ColorArg, escape, escape::warn, print, search};

/// The exit-code contract, in one place.
///
/// The whole point: a `1` must mean "read everything, found nothing". It is a
/// positive claim that the logs are clean, and during an incident someone acts
/// on it. Anything unread is a `2`, **even when matches were also found** —
/// "here are some matches, and there may be more you cannot see" is a warning,
/// not a success.
pub struct RunStatus {
    pub matched: bool,
    pub partial: bool,
}

impl RunStatus {
    pub fn exit_code(&self) -> i32 {
        if self.partial {
            2
        } else if self.matched {
            0
        } else {
            1
        }
    }
}

/// `main` deliberately does not return `anyhow::Result`.
///
/// Rust exits `1` when a `Result`-returning `main` returns `Err`, and `1` is
/// the one code trg may not guess at. A typo'd flag, an unparsable pattern or a
/// bad glob means **nothing was read at all**, so reporting it as "no matches,
/// everything read" is the exact failure this program exists to rule out. Every
/// fallible step therefore runs inside `run`, and every error out of it is a
/// `2`.
fn main() {
    let code = match run() {
        Ok(status) => status.exit_code(),
        Err(e) => {
            warn(&escape::for_stderr(&format!("{e:#}")));
            RunStatus { matched: false, partial: true }.exit_code()
        }
    };
    std::process::exit(code);
}

/// Which of stdin and stdout were closed when the process started, as bits 0
/// and 1.
///
/// Read before `main`, from a constructor, because by `main` it is too late:
/// Rust's runtime opens `/dev/null` on any of the three standard descriptors
/// it finds closed, so that a file opened later cannot land on one of them.
/// After that `trg line a.tgz >&-` wrote its lines to `/dev/null` and
/// exited 0, and nothing in `main` could tell it from `> /dev/null`, which
/// is a fair way to ask for the exit code alone.
static CLOSED_AT_START: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

extern "C" fn note_closed_fds() {
    let mut closed = 0;
    for fd in 0..2 {
        // The only failure F_GETFD has is EBADF: not an open descriptor.
        if unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
            closed |= 1 << fd;
        }
    }
    CLOSED_AT_START.store(closed, std::sync::atomic::Ordering::Relaxed);
}

// The C runtime calls every function listed in this section before `main`,
// and so before Rust's runtime starts. Where there is no such section the
// check is simply not made.
#[used]
#[cfg_attr(any(target_os = "linux", target_os = "android", target_os = "freebsd"), unsafe(link_section = ".init_array"))]
#[cfg_attr(target_vendor = "apple", unsafe(link_section = "__DATA,__mod_init_func"))]
static NOTE_CLOSED_FDS: extern "C" fn() = note_closed_fds;

fn closed_at_start(fd: u8) -> bool {
    CLOSED_AT_START.load(std::sync::atomic::Ordering::Relaxed) & (1 << fd) != 0
}

fn run() -> anyhow::Result<RunStatus> {
    // Output with nowhere to go is output lost, and that is a 2 before
    // anything is read. (A closed stderr is nobody's loss but its owner's.)
    if closed_at_start(1) {
        anyhow::bail!("standard output is closed: nothing could be written to it");
    }
    let mut args = trg::cli::parse()?;
    if closed_at_start(0) && args.paths.iter().any(|p| trg::source::is_stdin(p)) {
        anyhow::bail!("standard input is closed: there is nothing for - to read");
    }
    for flag in &args.deprecated {
        warn(&format!("{flag} is deprecated and has no effect: trg always streams since 0.7.0"));
    }

    // Stay out of the way before doing any work: a nice'd, idle-I/O process
    // uses spare capacity and is preempted the moment Apache wants the core.
    // Set once, so the workers inherit it rather than each re-applying it.
    //
    // The order is load-bearing: these two come before anything that starts a
    // thread, `load_gate` below included. On Linux neither is process-wide.
    // Niceness and the I/O class belong to the calling *thread*, and a new
    // thread copies them from the one that starts it, so a worker started
    // first would search at the priority trg was launched with.
    trg::resource::set_priority(args.nice);
    trg::resource::set_io_idle();
    // With --load-limit, all -j workers start and the gate decides how many may
    // search at once, re-read from the load average every few seconds. Without
    // it the gate is simply -j, and it never moves.
    let jobs_count = args.jobs;
    let gate = trg::resource::load_gate(jobs_count, args.load_limit)
        .unwrap_or_else(|| std::sync::Arc::new(trg::sched::Gate::new(jobs_count)));

    let matcher = search::build_matcher(&args.patterns, &args.search)?;

    // `auto` means "a person is looking at this", which is a tty and nothing
    // else. `trg ... | grep` and `trg ... > report.txt` must stay clean bytes.
    // Resolved once, here, so the printers and the buffer writer cannot
    // disagree about it.
    let color = match args.color {
        ColorArg::Always => true,
        ColorArg::Never => false,
        // `grep_cli::is_tty_stdout` is deprecated in favour of exactly this.
        ColorArg::Auto => std::io::IsTerminal::is_terminal(&std::io::stdout()),
    };
    args.print.color = color;
    // The same tty test as colour, but per stream: stderr is often a terminal
    // while stdout is a file, and diagnostics carry member names too.
    let escape_for = |tty: bool| match args.escape {
        ColorArg::Always => true,
        ColorArg::Never => false,
        ColorArg::Auto => tty,
    };
    args.print.escape = escape_for(std::io::IsTerminal::is_terminal(&std::io::stdout()));
    // A newline in a name is another matter from a control byte: it forges
    // a line for whatever reads the output, a pipe as much as a person. So
    // names are kept to one line unless `--escape never` asked for the bytes.
    let one_line_names = args.escape != ColorArg::Never;
    escape::set_stderr(escape_for(std::io::IsTerminal::is_terminal(&std::io::stderr())), one_line_names);
    let escape_stdout = args.print.escape;
    let bw = BufferWriter::stdout(if color { ColorChoice::Always } else { ColorChoice::Never });

    let mut matched = false;
    let mut partial = false;

    let archive_globs = trg::source::build_archive_globs(&args.archive_globs)?;
    let trg::source::Resolved { items, errors: path_errors, aliases } =
        trg::source::resolve_all(&args.paths, archive_globs.as_ref());
    let globs = trg::source::build_globs(&args.globs)?;
    for e in &path_errors {
        warn(&escape::diagnostic(e));
        partial = true;
    }

    let ctx = search::SearchCtx {
        matcher: &matcher,
        sep: args.archive_sep,
        globs: globs.as_ref(),
        drop_cache: args.drop_cache,
        aliases: &aliases,
        sink_limit: search::sink_limit(&args.search).map(|n| (n, args.search.after)),
        // Not for `-T`: a listing has no context to keep apart.
        separate: !args.list_members && search::separates_members(&args.search, args.print.mode),
        search_nothing: args.search.max_count == Some(0),
        // JSON has its own escape for a newline, and gets the name as it is.
        one_line_names: one_line_names && args.print.mode != print::Mode::Json,
    };
    // As the printer ends it, so one archive's `--` reads like the next's.
    let separator: &[u8] = if args.search.crlf { b"--\r\n" } else { b"--\n" };
    let mut printed_any = false;
    // `--json` ends with a summary record, as rg's does: the run's elapsed
    // time and its statistics summed over every archive.
    let started = std::time::Instant::now();
    let mut stats = grep_printer::Stats::new();
    let mut kept_any = false;
    let mut read_any = false;

    let jobs: Vec<trg::sched::Job> = items
        .iter()
        .enumerate()
        .map(|(i, p)| trg::sched::Job { index: i, path: p.clone() })
        .collect();

    let written = trg::sched::run_gated(
        jobs,
        trg::sched::Config { workers: jobs_count, sorted: args.sort, spill_bytes: 64 * 1024 * 1024 },
        &gate,
        || bw.buffer(),
        // Once per worker thread, never per archive: `Searcher` is not `Sync`,
        // so it cannot be shared, but its line buffer grows toward the heap
        // limit and rebuilding one per archive makes peak RSS climb with the
        // archive count.
        || search::build_searcher(&args.search),
        |job, searcher, out| {
            // The run's side of this job, taken before the printer borrows
            // the output for the length of the search.
            let ctl = out.control();
            // `-T` is a different question, not a different printer: it never
            // reads a member's *content*, so it takes its own path rather than
            // threading a "do not search" flag through the search. It still
            // opens the archive the same way, under the same cache policy.
            if args.list_members {
                let mut out = escape::Escaped::new(out, escape_stdout);
                return search::list_members(&ctx, &job.path, &ctl, &mut out);
            }
            let mut printer = print::build(&args.print, out);
            search::search_archive(&ctx, &job.path, searcher, &mut printer, &ctl)
        },
        // Called once per piece of an archive's output: once for most, and
        // for one that prints a great deal, with each segment as it fills,
        // `part` set on all but the last. Only the last carries the verdict.
        |done| {
            // From the sink's own verdict, never from "did this archive write
            // any bytes". `-q` matches and writes nothing; `-c` writes a line
            // whose content is a count. Output volume stopped being a proxy for
            // matching the moment those modes existed.
            if done.outcome.matched {
                matched = true;
            }
            kept_any |= done.outcome.kept;
            if let Some(st) = &done.outcome.stats {
                stats += st;
            }
            read_any |= !done.part && done.outcome.errors.is_empty();
            // The `--` between archives; `search_archive` writes the ones
            // between members of one archive. The rule is rg's, between
            // files: `--` goes before a file's first *line* once anything at
            // all has been printed. So an archive whose output starts with
            // `binary file matches` gets none before it, and still counts
            // as output for the archive after it.
            //
            // Decided on the archive's first piece, which may leave while
            // the archive is still being searched: the scheduler knows by
            // then what leads it.
            let mut r = Ok(());
            if ctx.separate && !done.continued && !done.buffer.is_empty() {
                if printed_any && done.leads_with_line {
                    let mut sep = bw.buffer();
                    r = std::io::Write::write_all(&mut sep, separator).and_then(|()| bw.print(&sep));
                }
                printed_any = true;
            }
            let r = r.and_then(|()| bw.print(&done.buffer));
            // Notes ride the same ordered path as errors so a sorted run says
            // the same thing twice in a row, but they leave `partial` alone:
            // nothing went unread, so the exit code is not theirs to change.
            for n in &done.outcome.notes {
                warn(&escape::diagnostic(n));
            }
            // Report this archive's own errors even if stdout has gone away:
            // stderr is usually still attached, and it is where the diagnosis
            // lives.
            //
            // `errors` being non-empty is the *only* per-archive partial-run
            // signal. Not `members_searched == 0`, and not `members_seen !=
            // members_searched`: one archive filtered down to nothing by `-g`
            // was read in full and is a clean 1. A whole run of them is not,
            // below.
            for e in &done.outcome.errors {
                warn(&escape::diagnostic(e));
                partial = true;
            }
            r
        },
    );

    // `-g` that kept nothing in the whole run is a 2, as `--archive-glob`
    // matching no files is. A typo such as `-g acess.log` searched nothing,
    // and a 1 would report the logs clean. Only once some archive was read
    // through: when every one failed, its own error is the whole story.
    if globs.is_some() && read_any && !kept_any {
        warn("-g matched no members in the archives searched");
        partial = true;
    }

    let written = written.and_then(|()| {
        // Not for `-m 0` either: rg searches nothing then, and says nothing.
        if args.print.mode != print::Mode::Json || args.list_members || args.search.max_count == Some(0) {
            return Ok(());
        }
        let elapsed = started.elapsed();
        let summary = serde_json::json!({
            "type": "summary",
            "data": {
                "elapsed_total": {
                    "secs": elapsed.as_secs(),
                    "nanos": elapsed.subsec_nanos(),
                    "human": format!("{:0.6}s", elapsed.as_secs_f64()),
                },
                "stats": stats,
            },
        });
        let mut buf = bw.buffer();
        std::io::Write::write_all(&mut buf, format!("{summary}\n").as_bytes())?;
        bw.print(&buf)
    });

    if let Err(e) = written {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            // `trg PATTERN *.tgz | head -5` is ordinary usage, not a fault. The
            // scheduler has already stopped the sweep; say nothing and report
            // on what was actually written.
        } else {
            // A genuine write failure — a full disk, say. Output that was lost
            // is output that went unseen, which is the same class of harm as
            // input that went unread, so it is a 2.
            warn(&format!("write error: {e}"));
            partial = true;
        }
    }

    Ok(RunStatus { matched, partial })
}

#[cfg(test)]
mod tests {
    use super::RunStatus;

    #[test]
    fn matches_with_everything_read_is_zero() {
        assert_eq!(RunStatus { matched: true, partial: false }.exit_code(), 0);
    }

    #[test]
    fn no_matches_with_everything_read_is_one() {
        assert_eq!(RunStatus { matched: false, partial: false }.exit_code(), 1);
    }

    #[test]
    fn anything_unread_is_two_even_with_no_matches() {
        assert_eq!(RunStatus { matched: false, partial: true }.exit_code(), 2);
    }

    /// The case that is easy to get wrong: matches *and* an unread archive.
    /// A 0 here would say "I searched everything and here it is".
    #[test]
    fn anything_unread_is_two_even_when_matches_were_found() {
        assert_eq!(RunStatus { matched: true, partial: true }.exit_code(), 2);
    }
}
