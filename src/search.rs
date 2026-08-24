//! Search configuration and the `-m` sink. Deliberately thin: the matching
//! engine is ripgrep's, unmodified.

use std::io;
use std::path::Path;

use grep_searcher::{
    BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkFinish, SinkMatch,
};

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

    /// The wrapped sink, so the caller can ask it what it saw.
    pub fn inner(&self) -> &S {
        &self.inner
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

    // `Sink` gives `begin` and `finish` default no-op implementations, so a
    // wrapper that forgets to forward them silently swallows them. That is not
    // cosmetic: `SummarySink` writes its whole result — the `-c` count line,
    // the `-l` path — from `finish`, and resets its per-search match count in
    // `begin`. Without these two, `-c` and `-l` print nothing at all.
    fn begin(&mut self, s: &Searcher) -> Result<bool, S::Error> {
        self.seen = 0;
        self.inner.begin(s)
    }

    fn finish(&mut self, s: &Searcher, f: &SinkFinish) -> Result<(), S::Error> {
        self.inner.finish(s, f)
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

/// Everything a search needs that is the same for every archive in a run.
///
/// Borrowed and shared across the whole scheduler: it is `Sync`, so one
/// instance is read by every worker. The per-job values — the path, the
/// `Searcher`, the printer, and the inflate strategy — stay as arguments,
/// because each worker must own its own (`Searcher` is not `Sync`) and the
/// strategy is chosen per file from its size.
pub struct SearchCtx<'a> {
    pub matcher: &'a grep_regex::RegexMatcher,
    pub sep: char,
    pub globs: Option<&'a globset::GlobSet>,
    pub max_count: Option<u64>,
    /// A request, not a decision: `open_decoded_dropping` narrows it to
    /// archives, so a plain live logfile is never evicted.
    pub drop_cache: bool,
}

/// Whether `-g` excludes this input, for the one case `for_each_member` cannot
/// decide on its own.
///
/// `-g` filters at the finest available granularity: tar member names for
/// archives, which `for_each_member` applies itself, and the file's own path
/// for plain (non-tar) files, which arrive here as a single member named `""`.
/// Without this second half, a mixed sweep of live logs plus archives would
/// ignore `-g` for every live log.
fn excluded_plain_file(member: &str, path: &Path, globs: Option<&globset::GlobSet>) -> bool {
    member.is_empty() && globs.is_some_and(|set| !set.is_match(path))
}

/// Search one member, returning whether it matched.
///
/// "Did this member match" comes from the sink's own `has_match()`, never from
/// whether bytes reached the buffer. Those two are only equal in the default
/// mode: `-q` matches and writes nothing, and `-c` can write a line for a
/// member whose interesting property is the count, not the bytes. Deriving the
/// exit code from output volume is how a run reports "no matches" for logs it
/// did match in.
fn search_member(
    printer: &mut crate::print::Printer<'_>,
    m: &grep_regex::RegexMatcher,
    s: &mut Searcher,
    r: &mut dyn io::Read,
    display: &str,
    max_count: Option<u64>,
) -> io::Result<bool> {
    // The three printers have unrelated sink types, so the body is monomorphic
    // per arm; a macro keeps the one real sequence — build sink, cap it,
    // search, ask it what it saw — written once.
    macro_rules! run {
        ($p:expr) => {{
            let sink = $p.sink_with_path(m, display);
            let mut capped = MaxCount::new(sink, max_count);
            let res = s.search_reader(m, r, &mut capped);
            // Read the verdict before propagating: a member that matched and
            // *then* hit a read error still matched, and the error is recorded
            // separately by the caller.
            let hit = capped.inner().has_match();
            res.map_err(io::Error::other)?;
            hit
        }};
    }

    Ok(match printer {
        crate::print::Printer::Standard(p) => run!(p),
        crate::print::Printer::Summary(p) => run!(p),
        crate::print::Printer::Json(p) => run!(p),
    })
}

/// Search one archive into `printer`'s buffer.
pub fn search_archive(
    ctx: &SearchCtx<'_>,
    path: &Path,
    s: &mut Searcher,
    printer: &mut crate::print::Printer<'_>,
    strategy: crate::inflate::Strategy,
) -> crate::archive::Outcome {
    let (m, sep, globs, max_count) = (ctx.matcher, ctx.sep, ctx.globs, ctx.max_count);
    let rdr = match crate::archive::open_decoded_dropping(path, strategy, ctx.drop_cache) {
        Ok(r) => r,
        Err(e) => {
            let mut o = crate::archive::Outcome::default();
            o.errors.push(format!("{}: {e}", path.display()));
            return o;
        }
    };

    let mut matched = false;
    let mut out = crate::archive::for_each_member(rdr, globs, |member, r| {
        if excluded_plain_file(member, path, globs) {
            return Ok(());
        }
        let display = display_path(path, member, sep);
        matched |= search_member(printer, m, s, r, &display, max_count)?;
        Ok(())
    });
    out.matched = matched;
    out
}

/// `-T/--list-members`: print member paths without searching.
///
/// Deliberately does **not** short-circuit on the first member: the exit-code
/// contract applies here too, so the whole archive is walked and any failure
/// on the way lands in `Outcome.errors`.
pub fn list_members(
    path: &Path,
    globs: Option<&globset::GlobSet>,
    sep: char,
    out: &mut dyn io::Write,
) -> crate::archive::Outcome {
    let rdr = match crate::archive::open_decoded(path) {
        Ok(r) => r,
        Err(e) => {
            let mut o = crate::archive::Outcome::default();
            o.errors.push(format!("{}: {e}", path.display()));
            return o;
        }
    };

    let mut listed = false;
    let mut outcome = crate::archive::for_each_member(rdr, globs, |member, _| {
        if excluded_plain_file(member, path, globs) {
            return Ok(());
        }
        listed = true;
        writeln!(out, "{}", display_path(path, member, sep))
    });
    // `-T` has no notion of a match, so "listed something" is what stands in
    // for it: `trg -T ... ; echo $?` should say 1 when an archive held nothing
    // the globs would accept, the same shape of answer as a search.
    outcome.matched = listed;
    outcome
}
