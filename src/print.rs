//! Printer construction. `path(true)` is mandatory: without it
//! `sink_with_path` silently degrades to `sink` and member names vanish.

use grep_printer::{Standard, StandardBuilder};
use termcolor::Buffer;

pub struct PrintOpts {
    pub heading: bool,
    pub line_numbers: bool,
    pub only_matching: bool,
    pub column: bool,
}

impl Default for PrintOpts {
    fn default() -> Self {
        Self { heading: false, line_numbers: true, only_matching: false, column: false }
    }
}

pub fn build<'a>(o: &PrintOpts, buf: &'a mut Buffer) -> Standard<&'a mut Buffer> {
    StandardBuilder::new()
        .path(true)
        .heading(o.heading)
        .only_matching(o.only_matching)
        .column(o.column)
        .stats(false)
        .build(buf)
}
