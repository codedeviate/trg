//! Search configuration and the `-m` sink. Deliberately thin: the matching
//! engine is ripgrep's, unmodified.

use std::io;
use std::path::Path;

use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};

pub struct SearchOpts {
    pub case_insensitive: bool,
    pub smart_case: bool,
    pub word: bool,
    pub fixed: bool,
    pub invert: bool,
    pub before: usize,
    pub after: usize,
    pub line_numbers: bool,
    /// `-a`: search members that look binary.
    pub text: bool,
    pub max_count: Option<u64>,
    pub heap_limit: usize,
    /// `--crlf`: treat CRLF as the line terminator. Off by default — see
    /// `agrees_with_rg_on_anchored_patterns` in `tests/differential.rs` for
    /// why this must never be hard-coded on.
    pub crlf: bool,
}

impl Default for SearchOpts {
    fn default() -> Self {
        Self {
            case_insensitive: false,
            smart_case: false,
            word: false,
            fixed: false,
            invert: false,
            before: 0,
            after: 0,
            line_numbers: true,
            text: false,
            max_count: None,
            heap_limit: 16 * 1024 * 1024,
            crlf: false,
        }
    }
}

pub fn build_matcher(
    pats: &[String],
    o: &SearchOpts,
) -> anyhow::Result<grep_regex::RegexMatcher> {
    let mut b = grep_regex::RegexMatcherBuilder::new();
    b.case_insensitive(o.case_insensitive)
        .case_smart(o.smart_case)
        .word(o.word)
        .fixed_strings(o.fixed)
        .multi_line(false)
        .crlf(o.crlf);
    Ok(b.build_many(pats)?)
}

pub fn build_searcher(o: &SearchOpts) -> Searcher {
    let mut b = SearcherBuilder::new();
    b.line_number(o.line_numbers)
        .before_context(o.before)
        .after_context(o.after)
        .invert_match(o.invert)
        .heap_limit(Some(o.heap_limit))
        .binary_detection(if o.text {
            BinaryDetection::none()
        } else {
            BinaryDetection::quit(b'\x00')
        });
    if o.crlf {
        b.line_terminator(grep_matcher::LineTerminator::crlf());
    }
    b.build()
}

/// `-m/--max-count`. `StandardBuilder` has no `max_matches`, so the limit is
/// enforced by wrapping the printer's sink and returning `Ok(false)` from
/// `matched`, which stops the search.
pub struct MaxCount<S> {
    inner: S,
    limit: Option<u64>,
    seen: u64,
}

impl<S: Sink> MaxCount<S> {
    pub fn new(inner: S, limit: Option<u64>) -> Self {
        Self { inner, limit, seen: 0 }
    }
}

impl<S: Sink> Sink for MaxCount<S> {
    type Error = S::Error;

    fn matched(&mut self, s: &Searcher, m: &SinkMatch<'_>) -> Result<bool, S::Error> {
        if !self.inner.matched(s, m)? {
            return Ok(false);
        }
        self.seen += 1;
        Ok(match self.limit {
            Some(n) => self.seen < n,
            None => true,
        })
    }

    fn context(&mut self, s: &Searcher, c: &SinkContext<'_>) -> Result<bool, S::Error> {
        self.inner.context(s, c)
    }

    fn context_break(&mut self, s: &Searcher) -> Result<bool, S::Error> {
        self.inner.context_break(s)
    }

    fn binary_data(&mut self, s: &Searcher, off: u64) -> Result<bool, S::Error> {
        self.inner.binary_data(s, off)
    }
}

/// `archive.tgz:logs/vhost03.access.log`, or just the path for a plain file.
pub fn display_path(archive: &Path, member: &str, sep: char) -> String {
    if member.is_empty() {
        archive.display().to_string()
    } else {
        format!("{}{}{}", archive.display(), sep, member)
    }
}

/// Search one archive into `printer`'s buffer.
pub fn search_archive(
    path: &Path,
    m: &grep_regex::RegexMatcher,
    s: &mut Searcher,
    printer: &mut grep_printer::Standard<&mut termcolor::Buffer>,
    sep: char,
    globs: Option<&globset::GlobSet>,
    max_count: Option<u64>,
) -> crate::archive::Outcome {
    let rdr = match crate::archive::open_decoded(path) {
        Ok(r) => r,
        Err(e) => {
            let mut o = crate::archive::Outcome::default();
            o.errors.push(format!("{}: {e}", path.display()));
            return o;
        }
    };

    crate::archive::for_each_member(rdr, globs, |member, r| {
        // `-g` filters at the finest available granularity: tar member names
        // for archives (handled inside `for_each_member`), and the file's
        // own path for plain (non-tar) files, which arrive here as a single
        // member named `""`. Without this, a mixed sweep of live logs plus
        // archives would ignore `-g` for every live log.
        if member.is_empty() {
            if let Some(set) = globs {
                if !set.is_match(path) {
                    return Ok(());
                }
            }
        }
        let display = display_path(path, member, sep);
        let sink = printer.sink_with_path(m, display.as_str());
        let mut capped = MaxCount::new(sink, max_count);
        s.search_reader(m, r, &mut capped).map_err(io::Error::other)
    })
}
