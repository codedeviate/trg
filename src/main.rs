use termcolor::{BufferWriter, ColorChoice};
use trg::{print, search};

fn main() -> anyhow::Result<()> {
    let args = trg::cli::parse()?;

    // Stay out of the way before doing any work: a nice'd, idle-I/O process
    // uses spare capacity and is preempted the moment Apache wants the core.
    trg::resource::set_priority(args.nice);
    trg::resource::set_io_idle();
    // Read once at startup, no feedback loop. Task 10's scheduler consumes
    // this; nothing here is concurrent yet.
    let _jobs = trg::resource::clamp_jobs(args.jobs, args.load_limit);

    let matcher = search::build_matcher(&args.patterns, &args.search)?;
    let mut searcher = search::build_searcher(&args.search);

    let bw = BufferWriter::stdout(ColorChoice::Never);
    let mut matched = false;
    let mut partial = false;

    let (items, path_errors) = trg::source::resolve(&args.paths);
    let globs = trg::source::build_globs(&args.globs)?;
    for e in &path_errors {
        eprintln!("trg: {e}");
        partial = true;
    }

    for p in &items {
        let mut buf = bw.buffer();
        let mut printer = print::build(&args.print, &mut buf);
        let strategy = trg::inflate::choose(args.inflate, p, args.inflate_budget);
        let outcome = search::search_archive(
            p,
            &matcher,
            &mut searcher,
            &mut printer,
            args.archive_sep,
            globs.as_ref(),
            args.search.max_count,
            strategy,
            args.drop_cache,
        );
        if printer.has_written() {
            matched = true;
        }
        drop(printer);
        bw.print(&buf)?;
        for e in &outcome.errors {
            eprintln!("trg: {e}");
            partial = true;
        }
    }

    std::process::exit(if partial { 2 } else if matched { 0 } else { 1 });
}
