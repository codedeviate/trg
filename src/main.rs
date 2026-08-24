use termcolor::{BufferWriter, ColorChoice};
use trg::{cli::ColorArg, print, search};

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
            eprintln!("trg: {e:#}");
            RunStatus { matched: false, partial: true }.exit_code()
        }
    };
    std::process::exit(code);
}

fn run() -> anyhow::Result<RunStatus> {
    let mut args = trg::cli::parse()?;

    // Stay out of the way before doing any work: a nice'd, idle-I/O process
    // uses spare capacity and is preempted the moment Apache wants the core.
    // Set once, process-wide, so the workers inherit it rather than each
    // re-applying it.
    trg::resource::set_priority(args.nice);
    trg::resource::set_io_idle();
    // Read once at startup, no feedback loop.
    let jobs_count = trg::resource::clamp_jobs(args.jobs, args.load_limit);

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
    let bw = BufferWriter::stdout(if color { ColorChoice::Always } else { ColorChoice::Never });

    let mut matched = false;
    let mut partial = false;

    let (items, path_errors) = trg::source::resolve(&args.paths);
    let globs = trg::source::build_globs(&args.globs)?;
    for e in &path_errors {
        eprintln!("trg: {e}");
        partial = true;
    }

    let ctx = search::SearchCtx {
        matcher: &matcher,
        sep: args.archive_sep,
        globs: globs.as_ref(),
        max_count: args.search.max_count,
        drop_cache: args.drop_cache,
    };

    let jobs: Vec<trg::sched::Job> = items
        .iter()
        .enumerate()
        .map(|(i, p)| trg::sched::Job { index: i, path: p.clone() })
        .collect();

    let written = trg::sched::run(
        jobs,
        trg::sched::Config { workers: jobs_count, sorted: args.sort, spill_bytes: 64 * 1024 * 1024 },
        || bw.buffer(),
        // Once per worker thread, never per archive: `Searcher` is not `Sync`,
        // so it cannot be shared, but its line buffer grows toward the heap
        // limit and rebuilding one per archive makes peak RSS climb with the
        // archive count.
        || search::build_searcher(&args.search),
        |job, searcher, buf| {
            // `-T` is a different question, not a different printer: it never
            // decompresses a member, so it takes its own path rather than
            // threading a "do not search" flag through the search.
            if args.list_members {
                return search::list_members(&job.path, globs.as_ref(), args.archive_sep, buf);
            }
            let mut printer = print::build(&args.print, buf);
            let strategy = trg::inflate::choose(args.inflate, &job.path, args.inflate_budget);
            search::search_archive(&ctx, &job.path, searcher, &mut printer, strategy)
        },
        |done| {
            // From the sink's own verdict, never from "did this archive write
            // any bytes". `-q` matches and writes nothing; `-c` writes a line
            // whose content is a count. Output volume stopped being a proxy for
            // matching the moment those modes existed.
            if done.outcome.matched {
                matched = true;
            }
            let r = bw.print(&done.buffer);
            // Report this archive's own errors even if stdout has gone away:
            // stderr is usually still attached, and it is where the diagnosis
            // lives.
            //
            // `errors` being non-empty is the *only* partial-run signal. Not
            // `members_searched == 0`, and not `members_seen != members_searched`
            // — an archive filtered down to nothing by `-g` was read in full and
            // is a clean 1.
            for e in &done.outcome.errors {
                eprintln!("trg: {e}");
                partial = true;
            }
            r
        },
    );

    if let Err(e) = written {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            // `trg PATTERN *.tgz | head -5` is ordinary usage, not a fault. The
            // scheduler has already stopped the sweep; say nothing and report
            // on what was actually written.
        } else {
            // A genuine write failure — a full disk, say. Output that was lost
            // is output that went unseen, which is the same class of harm as
            // input that went unread, so it is a 2.
            eprintln!("trg: write error: {e}");
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
