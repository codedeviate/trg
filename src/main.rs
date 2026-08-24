use std::path::PathBuf;

use termcolor::{BufferWriter, ColorChoice};
use trg::{print, search};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let pattern = args.next().expect("usage: trg PATTERN PATH...").into_string().unwrap();
    let paths: Vec<PathBuf> = args.map(PathBuf::from).collect();

    let opts = search::SearchOpts::default();
    let matcher = search::build_matcher(&[pattern], &opts)?;
    let mut searcher = search::build_searcher(&opts);

    let bw = BufferWriter::stdout(ColorChoice::Never);
    let mut matched = false;
    let mut partial = false;

    for p in &paths {
        let mut buf = bw.buffer();
        let mut printer = print::build(&print::PrintOpts::default(), &mut buf);
        let outcome = search::search_archive(
            p, &matcher, &mut searcher, &mut printer, ':', None, None,
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
