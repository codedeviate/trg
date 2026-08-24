//! Printer construction and output-mode dispatch. `path(true)` is mandatory:
//! without it `sink_with_path` silently degrades to `sink` and member names
//! vanish.

use grep_printer::{
    ColorSpecs, Standard, StandardBuilder, Summary, SummaryBuilder, SummaryKind, JSON, JSONBuilder,
};
use termcolor::Buffer;

/// The mutually exclusive output modes.
///
/// Mutually exclusive because each one is a different answer to "what is a
/// result": a line, a count, a path, or nothing at all. `cli` collapses the
/// flags into exactly one of these, so nothing downstream has to re-derive
/// precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// One line per match — the default.
    Standard,
    /// `-c`: one `member:count` line per member that matched.
    Count,
    /// `-l`: one `member` line per member that matched.
    FilesWithMatches,
    /// `-q`: nothing at all; the exit code is the whole report.
    Quiet,
    /// `--json`: one JSON object per event, JSON Lines.
    Json,
}

pub struct PrintOpts {
    pub heading: bool,
    pub line_numbers: bool,
    pub only_matching: bool,
    pub column: bool,
    pub mode: Mode,
    /// Resolved by `main` from `--color` plus whether stdout is a tty; by the
    /// time it reaches here it is a decision, not a preference.
    pub color: bool,
}

impl Default for PrintOpts {
    fn default() -> Self {
        Self {
            heading: false,
            line_numbers: true,
            only_matching: false,
            column: false,
            mode: Mode::Standard,
            color: false,
        }
    }
}

/// One archive's printer, in whichever mode the run asked for.
///
/// An enum rather than a trait object because the three printers' sinks are
/// unrelated concrete types with different lifetimes, and boxing them would
/// buy nothing: the match arms live in exactly one place, `search::search_member`.
pub enum Printer<'a> {
    Standard(Standard<&'a mut Buffer>),
    Summary(Summary<&'a mut Buffer>),
    Json(JSON<&'a mut Buffer>),
}

pub fn build<'a>(o: &PrintOpts, buf: &'a mut Buffer) -> Printer<'a> {
    // On a `Buffer` built for a `ColorChoice::Never` writer these specs are
    // inert, so passing them unconditionally would still be correct; gating
    // keeps the "did the operator ask for colour" decision visible in one
    // place instead of implied by the buffer's construction.
    let specs =
        if o.color { ColorSpecs::default_with_color() } else { ColorSpecs::default() };

    match o.mode {
        Mode::Standard => Printer::Standard(
            StandardBuilder::new()
                .path(true)
                .heading(o.heading)
                .only_matching(o.only_matching)
                .column(o.column)
                .stats(false)
                .color_specs(specs)
                .build(buf),
        ),
        // The JSON schema carries the path, line number and offsets itself, so
        // none of the standard printer's layout options apply.
        Mode::Json => Printer::Json(JSONBuilder::new().build(buf)),
        // `-c`, `-l` and `-q` are all "one aggregate result per member", which
        // is exactly what `Summary` is for. Writing the count line by hand
        // instead would mean re-deriving `exclude_zero`, the field separator
        // and the path colouring that this printer already gets right.
        //
        // `stats` stays off: none of these modes needs the extra statistics,
        // and enabling it would force `QuietWithMatch` to keep searching a
        // member after the first match purely to compute numbers nobody reads.
        Mode::Count | Mode::FilesWithMatches | Mode::Quiet => {
            let kind = match o.mode {
                Mode::Count => SummaryKind::Count,
                Mode::FilesWithMatches => SummaryKind::PathWithMatch,
                _ => SummaryKind::QuietWithMatch,
            };
            Printer::Summary(
                SummaryBuilder::new()
                    .kind(kind)
                    .path(true)
                    .stats(false)
                    .color_specs(specs)
                    .build(buf),
            )
        }
    }
}
