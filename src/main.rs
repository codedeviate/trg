use termcolor::{BufferWriter, ColorChoice};
use trg::{print, search};

fn main() -> anyhow::Result<()> {
    let args = trg::cli::parse()?;

    // Stay out of the way before doing any work: a nice'd, idle-I/O process
    // uses spare capacity and is preempted the moment Apache wants the core.
    // Set once, process-wide, so the workers inherit it rather than each
    // re-applying it.
    trg::resource::set_priority(args.nice);
    trg::resource::set_io_idle();
    // Read once at startup, no feedback loop.
    let jobs_count = trg::resource::clamp_jobs(args.jobs, args.load_limit);

    let matcher = search::build_matcher(&args.patterns, &args.search)?;

    let bw = BufferWriter::stdout(ColorChoice::Never);
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
            let mut printer = print::build(&args.print, buf);
            let strategy = trg::inflate::choose(args.inflate, &job.path, args.inflate_budget);
            search::search_archive(&ctx, &job.path, searcher, &mut printer, strategy)
        },
        |done| {
            // The printer only ever writes on a match, so a non-empty buffer
            // is exactly "this archive matched".
            if !done.buffer.is_empty() {
                matched = true;
            }
            let r = bw.print(&done.buffer);
            // Report this archive's own errors even if stdout has gone away:
            // stderr is usually still attached, and it is where the diagnosis
            // lives.
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
            // A genuine write failure — a full disk, say. Reporting success
            // while output was silently lost is the one outcome worth ruling
            // out.
            eprintln!("trg: write error: {e}");
            partial = true;
        }
    }

    std::process::exit(if partial { 2 } else if matched { 0 } else { 1 });
}
