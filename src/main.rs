use termcolor::{BufferWriter, ColorChoice};
use trg::{print, search};

fn main() -> anyhow::Result<()> {
    let args = trg::cli::parse()?;
    let matcher = search::build_matcher(&args.patterns, &args.search)?;
    let mut searcher = search::build_searcher(&args.search);

    let bw = BufferWriter::stdout(ColorChoice::Never);
    let mut matched = false;
    let mut partial = false;

    for p in &args.paths {
        let mut buf = bw.buffer();
        let mut printer = print::build(&args.print, &mut buf);
        let outcome = search::search_archive(
            p,
            &matcher,
            &mut searcher,
            &mut printer,
            args.archive_sep,
            None,
            args.search.max_count,
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
