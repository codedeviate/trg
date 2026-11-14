//! Search configuration and the `-m` sink. Deliberately thin: the matching
//! engine is ripgrep's, unmodified.

use std::io;
use std::path::{Path, PathBuf};

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
    // Built as ripgrep builds it: multi-line, so `^` and `$` hold at every line
    // and not only at the edges of whatever buffer the searcher hands over,
    // with `\n` as the line terminator so no match can span two lines. Built
    // single-line, `-o '^[0-9.]+'` matched the first line and printed every
    // later hit whole.
    //
    // `crlf` goes first: `crlf(false)` clears the line terminator, and set
    // the other way round the matcher had none. That sent every search down
    // the searcher's slow line-by-line path, where `-v -m N -A k` printed
    // every line after the limit as a match (a grep-searcher bug rg also
    // hits, with `\A` patterns: ripgrep#3593). `crlf(true)` sets `\r\n`
    // itself.
    let mut b = grep_regex::RegexMatcherBuilder::new();
    b.case_insensitive(o.case_insensitive)
        .case_smart(o.smart_case)
        .word(o.word)
        .fixed_strings(o.fixed)
        .multi_line(true)
        .crlf(o.crlf);
    if !o.crlf {
        b.line_terminator(Some(b'\n'));
    }
    Ok(b.build_many(pats)?)
}

pub fn build_searcher(o: &SearchOpts) -> Searcher {
    let mut b = SearcherBuilder::new();
    b.line_number(o.line_numbers)
        .before_context(o.before)
        .after_context(o.after)
        .invert_match(o.invert)
        // `-m`, enforced by the searcher as in rg: it stops after the limit
        // but still prints the after-context of the last match, and it counts
        // inverted matches right (`-v -m`). Except for `-v -m` with
        // after-context, which `sink_limit` hands to the sink.
        .max_matches(if sink_limit(o).is_some() { None } else { o.max_count })
        .heap_limit(Some(o.heap_limit))
        // Read on past binary data, as rg reads a file it is handed
        // (`rg --binary` for one it finds): a NUL ends a line, the search
        // goes on, and a match after it prints `binary file matches`.
        // Stopping at the first NUL, as rg does for a file it finds, made a
        // 1 that had not read everything: a log rotated by copytruncate,
        // reopened by a writer at its old offset, starts with a hole of NULs
        // and its lines after it were never searched.
        .binary_detection(if o.text {
            BinaryDetection::none()
        } else {
            BinaryDetection::convert(b'\x00')
        });
    if o.crlf {
        b.line_terminator(grep_matcher::LineTerminator::crlf());
    }
    b.build()
}

/// The `-m` limit the sink enforces instead of the searcher: `-v -m N` with
/// after-context. grep-searcher 0.1.17 gets that one wrong. Once the limit is
/// reached it searches the rest of a buffer as if no line matched, so with
/// `-v` every line from there on is an inverted match. That happens on its
/// slow path, and on the fast one when the after-context runs into the next
/// read buffer, which streamed members do all the time: every line after the
/// limit came out as a match. rg 15.2 does the same reading a pipe. With no
/// limit of its own, the searcher tells matches from context right.
/// Reported upstream as <https://github.com/BurntSushi/ripgrep/issues/3593>;
/// once fixed, this and the limit in [`MaxCount`] can go.
pub fn sink_limit(o: &SearchOpts) -> Option<u64> {
    o.max_count.filter(|_| o.invert && o.after > 0)
}

/// Wraps the printer's sink to record what the searcher reported, which is
/// the exit code's source of truth. Named for the `-m` limit it used to
/// enforce; `SearcherBuilder::max_matches` does that now, as in rg, because
/// stopping the search from the sink cut off `-m`'s after-context and let
/// `-v -m` print a line past the limit.
pub struct MaxCount<S> {
    inner: S,
    seen: u64,
    /// [`sink_limit`], and the after-context it allows past it.
    limit: Option<(u64, usize)>,
    /// Lines still to print once the limit is reached, as rg prints them:
    /// matches as matches, the rest as context. At `Some(0)` every call is
    /// refused unforwarded: the searcher's fast path takes a sink's `false`
    /// for the end of one loop and goes on to look for after-context.
    left: Option<usize>,
    /// Print `--` before this member's first line, as rg does between files
    /// when context is on. Written through the printer's own `context_break`,
    /// so it is the same separator a gap inside one member gets.
    separate: bool,
    /// Whether anything was printed for this member: a line, or `binary file
    /// matches`.
    printed: bool,
    /// Whether a line went to the printer for this member.
    line: bool,
    /// The printer is the default one, which prints no line after binary
    /// data, only `binary file matches` once the search is over.
    standard: bool,
    /// Binary data has been seen in this member.
    binary: bool,
}

impl<S: Sink> MaxCount<S> {
    pub fn new(inner: S, limit: Option<(u64, usize)>, separate: bool, standard: bool) -> Self {
        Self { inner, seen: 0, limit, left: None, separate, printed: false, line: false, standard, binary: false }
    }

    /// The default printer, past binary data: it takes no more lines.
    ///
    /// It refuses a context line there by returning `false`, which stops the
    /// search, so with `-B` the line before a match ended the search before
    /// the match was reported: a 1 for a member that matched. rg 15.2 does
    /// the same (`rg --binary -B1`), and plain `rg --binary` finds the match.
    /// Such lines are dropped here instead, and the search goes on to the
    /// match, which the printer reports as `binary file matches`. After a
    /// match it stops as before: the outcome is settled.
    fn mute(&self) -> bool {
        self.standard && self.binary
    }

    /// Whether anything was printed for this member.
    pub fn printed(&self) -> bool {
        self.printed
    }

    /// Whether a line was printed for this member, and so ahead of anything
    /// else it printed: `binary file matches` comes last.
    pub fn printed_line(&self) -> bool {
        self.line
    }

    /// Called before every line goes to the printer.
    fn before_line(&mut self, s: &Searcher) -> Result<(), S::Error> {
        if self.mute() {
            return Ok(());
        }
        if !self.printed {
            self.printed = true;
            self.line = true;
            if self.separate {
                self.inner.context_break(s)?;
            }
        }
        Ok(())
    }

    fn done(&self) -> bool {
        self.left == Some(0)
    }

    /// One more line past the limit; whether the search goes on.
    fn past_limit(&mut self, more: bool) -> bool {
        match self.left.as_mut() {
            Some(left) => {
                *left -= 1;
                more && *left > 0
            }
            None => more,
        }
    }

    /// Whether the searcher reported any match at all for this member.
    ///
    /// This, and not the printer's own summary, is the exit code's source of
    /// truth. See the note on `matched` below for why the difference matters.
    pub fn saw_match(&self) -> bool {
        self.seen > 0
    }
}

impl<S: Sink> Sink for MaxCount<S> {
    type Error = S::Error;

    fn matched(&mut self, s: &Searcher, m: &SinkMatch<'_>) -> Result<bool, S::Error> {
        // Counted *before* the inner sink is consulted, and this ordering is
        // load-bearing twice over.
        //
        // A printer is allowed to say "stop, I have seen enough" — `-l` and
        // `-q` both quit on their first match, and the default printer at
        // the first match after binary data — and a printer is allowed to
        // revise what it reports afterwards.
        //
        // Counting first makes `seen` the record of what the *searcher*
        // reported, which no printer can overrule and no early stop can
        // truncate.
        if self.done() {
            return Ok(false);
        }
        self.before_line(s)?;
        if self.left.is_some() {
            let more = self.inner.matched(s, m)?;
            return Ok(self.past_limit(more));
        }
        self.seen += 1;
        let more = self.inner.matched(s, m)?;
        match self.limit {
            Some((n, after)) if self.seen == n => {
                self.left = Some(after);
                Ok(more && after > 0)
            }
            _ => Ok(more),
        }
    }

    fn context(&mut self, s: &Searcher, c: &SinkContext<'_>) -> Result<bool, S::Error> {
        if self.done() {
            return Ok(false);
        }
        self.before_line(s)?;
        // Past binary data, a line before the member's first match is
        // stepped over so the search reaches the match. Once it has
        // matched, the outcome is settled and the search stops, as rg's does.
        let more = if self.mute() { self.seen == 0 } else { self.inner.context(s, c)? };
        Ok(self.past_limit(more))
    }

    fn context_break(&mut self, s: &Searcher) -> Result<bool, S::Error> {
        if self.done() {
            return Ok(false);
        }
        // Not muted: a gap after this member's lines gets its `--` before
        // `binary file matches`, as in rg.
        self.inner.context_break(s)
    }

    fn binary_data(&mut self, s: &Searcher, off: u64) -> Result<bool, S::Error> {
        self.binary = true;
        self.inner.binary_data(s, off)
    }

    // `Sink` gives `begin` and `finish` default no-op implementations, so a
    // wrapper that forgets to forward them silently swallows them. That is not
    // cosmetic: `SummarySink` writes its whole result — the `-c` count line,
    // the `-l` path — from `finish`, and resets its per-search match count in
    // `begin`. Without these two, `-c` and `-l` print nothing at all.
    fn begin(&mut self, s: &Searcher) -> Result<bool, S::Error> {
        self.seen = 0;
        self.binary = false;
        self.left = None;
        self.printed = false;
        self.line = false;
        self.inner.begin(s)
    }

    fn finish(&mut self, s: &Searcher, f: &SinkFinish) -> Result<(), S::Error> {
        // The printer writes `binary file matches` for a member that
        // matched; the next member's `--` goes after it, as in rg.
        if self.mute() && self.seen > 0 {
            self.printed = true;
        }
        self.inner.finish(s, f)
    }
}

/// `archive.tgz:logs/vhost03.access.log`, or just the path for a plain file.
pub fn display_path(archive: &Path, member: &str, sep: char) -> String {
    if member.is_empty() {
        crate::source::display(archive)
    } else {
        format!("{}{}{}", crate::source::display(archive), sep, member)
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
    pub globs: Option<&'a crate::source::Globs>,
    /// A request, not a decision: `walk_archive` narrows it to
    /// archives, so a plain live logfile is never evicted.
    pub drop_cache: bool,
    /// Other names of the files searched, for `-g` on plain files.
    pub aliases: &'a crate::source::Aliases,
    /// `-m` and `-A`, when the sink enforces `-m`: see [`sink_limit`].
    pub sink_limit: Option<(u64, usize)>,
    /// `--` between members, as rg puts it between files: the default output
    /// with context on. See [`separates_members`].
    pub separate: bool,
    /// `-m 0`: search nothing, as rg does. See [`search_archive`].
    pub search_nothing: bool,
    /// Show a newline in a name as `\x0a`: see [`crate::escape::name`]. Off
    /// for `--escape never`, and for `--json`, which has its own escapes.
    pub one_line_names: bool,
}

impl SearchCtx<'_> {
    /// A member's name as the output shows it.
    fn shown(&self, display: String) -> String {
        match crate::escape::name(&display) {
            std::borrow::Cow::Owned(s) if self.one_line_names => s,
            _ => display,
        }
    }
}

/// Whether the output puts `--` between members, as rg does between files.
/// Without it, `-C` output from two members runs together, and the last
/// lines of one read as the context of the first match in the next.
pub fn separates_members(o: &SearchOpts, mode: crate::print::Mode) -> bool {
    mode == crate::print::Mode::Standard && (o.before > 0 || o.after > 0)
}

/// Whether `-g` excludes this input, for the one case `for_each_member` cannot
/// decide on its own.
///
/// `-g` filters at the finest available granularity: tar member names for
/// archives, which `for_each_member` applies itself, and the file's own path
/// for plain (non-tar) files, which arrive here as a single member named `""`.
/// Without this second half, a mixed sweep of live logs plus archives would
/// ignore `-g` for every live log.
///
/// Returns the name to show it under, `None` when it is excluded. A plain file
/// reached by several names (a symlink, a hardlink) was searched under the
/// first, and `-g` may pick another: then it is searched, under that one.
fn plain_name<'p>(member: &str, path: &'p Path, ctx: &'p SearchCtx<'_>) -> Option<&'p Path> {
    // Standard input has no name for a glob to judge, and asking for it by
    // `-` is naming it.
    let Some(set) = ctx.globs.filter(|_| member.is_empty() && !crate::source::is_stdin(path)) else {
        return Some(path);
    };
    let others = ctx.aliases.get(path).map(Vec::as_slice).unwrap_or_default();
    std::iter::once(path)
        .chain(others.iter().map(PathBuf::as_path))
        .find(|p| set.is_match(crate::source::glob_candidate(p)))
}

/// Fills the searcher's buffer the way a plain file does: every read returns
/// as much as was asked for, short only at the end of the member.
///
/// The searcher looks for binary data one buffer at a time and stops at the
/// first NUL, keeping what it matched before it. gzip and xz hand back short
/// reads, so the first buffer of a member held less of it than with zstd, a
/// plain tar or the file itself, and the same member with a NUL a few KB in
/// was a 0 with matches in a `.tgz` and a clean 1 in a `.tar.zst`.
struct FullReads<'a> {
    inner: &'a mut dyn io::Read,
    /// An error met after part of a buffer was read: that part goes out
    /// first, and the error with the next call. Held here, not left to the
    /// reader to repeat, because a decoder need not say it twice, and an
    /// error lost here is a 1 about a member never read to its end.
    pending: Option<io::Error>,
    /// The member's first bytes, to tell an archive inside the archive.
    head: Vec<u8>,
}

impl<'a> FullReads<'a> {
    fn new(inner: &'a mut dyn io::Read) -> Self {
        Self { inner, pending: None, head: Vec::new() }
    }
}

impl io::Read for FullReads<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Some(e) = self.pending.take() {
            return Err(e);
        }
        let mut n = 0;
        while n < buf.len() {
            match self.inner.read(&mut buf[n..]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if n > 0 => {
                    self.pending = Some(e);
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        if self.head.len() < crate::sniff::SNIFF_LEN {
            let more = (crate::sniff::SNIFF_LEN - self.head.len()).min(n);
            self.head.extend_from_slice(&buf[..more]);
        }
        Ok(n)
    }
}

/// What one member's search concluded, beyond the bytes it printed.
pub struct MemberVerdict {
    pub matched: bool,
    /// Anything was printed for it.
    pub printed: bool,
    /// What was printed starts with a line, and is not `binary file matches`
    /// alone.
    pub line: bool,
    /// With `--json`, the printer's statistics for the member.
    pub stats: Option<grep_printer::Stats>,
    /// The member is itself an archive, `"zip"` or `"tar"`, whose members
    /// were not searched.
    pub container: Option<&'static str>,
    /// What was searched is itself compressed, in this format: the level
    /// under the one that was removed.
    pub compressed: Option<&'static str>,
}

/// Search one member, returning whether it matched.
///
/// "Did this member match" comes from [`MaxCount::saw_match`] — the searcher's
/// own tally — and from neither of the two tempting alternatives. Not from
/// whether bytes reached the buffer: `-q` matches and writes nothing, and `-c`
/// writes a line whose content is a count. And not from the sink's
/// `has_match()` either, because `SummarySink` retracts that after seeing
/// binary data, which would make `-c` exit 1 on an archive `-l`, `-q`,
/// `--json` and the default mode all agree matched.
fn search_member(
    printer: &mut crate::print::Printer<'_>,
    m: &grep_regex::RegexMatcher,
    s: &mut Searcher,
    r: &mut dyn io::Read,
    display: &str,
    limit: Option<(u64, usize)>,
    separate: bool,
) -> io::Result<MemberVerdict> {
    // The three printers have unrelated sink types, so the body is monomorphic
    // per arm; a macro keeps the one real sequence — build sink, cap it,
    // search, ask it what it saw — written once. `$stats` reads the sink's
    // statistics, which only the JSON printer keeps.
    macro_rules! run {
        ($p:expr, $standard:expr, |$sink:ident| $stats:expr) => {{
            let sink = $p.sink_with_path(m, display);
            let mut capped = MaxCount::new(sink, limit, separate, $standard);
            let mut reads = FullReads::new(&mut *r);
            let res = s.search_reader(m, &mut reads, &mut capped);
            let head = crate::sniff::sniff(&reads.head);
            let container = match head {
                crate::sniff::Format::Zip => Some("zip"),
                crate::sniff::Format::Tar => Some("tar"),
                _ => None,
            };
            let compressed = still_compressed(head);
            // Read the verdict before propagating: a member that matched and
            // *then* hit a read error still matched, and the error is recorded
            // separately by the caller.
            let hit = capped.saw_match();
            let printed = capped.printed();
            let line = capped.printed_line();
            res.map_err(io::Error::other)?;
            let $sink = &capped.inner;
            MemberVerdict { matched: hit, printed, line, stats: $stats, container, compressed }
        }};
    }

    Ok(match printer {
        crate::print::Printer::Standard(p) => run!(p, true, |_sink| None),
        crate::print::Printer::Summary(p) => run!(p, false, |_sink| None),
        crate::print::Printer::Json(p) => run!(p, false, |sink| Some(sink.stats().clone())),
    })
}

/// The name of a compression format, for data that sniffs as one after a
/// level was already removed. The same sniff that opens a file: whatever
/// trg would have decompressed at the top level, it has not read here.
fn still_compressed(head: crate::sniff::Format) -> Option<&'static str> {
    use crate::sniff::Format;
    match head {
        Format::Gzip => Some("gzip"),
        Format::Bzip2 => Some("bzip2"),
        Format::Xz => Some("xz"),
        Format::Zstd => Some("zstd"),
        Format::Lz4 => Some("lz4"),
        Format::CompressZ => Some(".Z"),
        // An unreadable format is refused where the stream is opened.
        Format::Zip | Format::Tar | Format::Plain | Format::Unreadable(_) => None,
    }
}

/// Search one archive into `printer`'s buffer.
pub fn search_archive(
    ctx: &SearchCtx<'_>,
    path: &Path,
    s: &mut Searcher,
    printer: &mut crate::print::Printer<'_>,
) -> crate::archive::Outcome {
    let (m, sep, globs) = (ctx.matcher, ctx.sep, ctx.globs);

    let mut matched = false;
    // Whether this archive has printed a line yet: the members after the
    // first that print get a `--` ahead of them. Between archives, `main`
    // writes it.
    let mut printed = false;
    // Whether the first thing it printed is a line: `main` puts its `--`
    // before a line, never before a lone `binary file matches`.
    let mut leads_with_line = false;
    let mut kept = false;
    let mut stats: Option<grep_printer::Stats> = None;
    let mut notes: Vec<String> = Vec::new();
    let mut unread: Vec<String> = Vec::new();
    let mut out = crate::archive::walk_archive(path, ctx.drop_cache, globs, |member, r| {
        let Some(shown) = plain_name(member, path, ctx) else {
            return Ok(crate::archive::Rest::Leave);
        };
        let display = ctx.shown(display_path(shown, member, sep));
        kept = true;
        // `-m 0` asks for no match at all, and rg answers it by searching
        // nothing: no line, no count, a 1. The searcher cannot be asked: with
        // `-v` it reports one match before it looks at a limit of 0, and
        // with `-A` as well, every line. The member is still read to its
        // end below, so a cut archive is a 2 as under any other flag.
        if ctx.search_nothing {
            return Ok(crate::archive::Rest::Verify);
        }
        let separate = ctx.separate && printed;
        let v = search_member(printer, m, s, r, &display, ctx.sink_limit, separate)?;
        matched |= v.matched;
        if !printed && v.printed {
            leads_with_line = v.line;
        }
        printed |= v.printed;
        if let Some(st) = &v.stats {
            *stats.get_or_insert_with(grep_printer::Stats::new) += st;
        }
        // Archives inside archives are not opened (OUT-OF-SCOPE.md). Their
        // bytes are searched as one binary file, which finds a line in a
        // plain tar but nothing in a compressed zip entry, and shows a hit
        // only as `binary file matches`. A tar is the likelier: `tar -cf
        // month.tar day*.tgz` holds tgzs, unwrapped here to tars.
        match v.container {
            Some("zip") => notes.push(format!("{display}: a zip inside the archive; its entries are not opened")),
            Some(kind) => notes.push(format!(
                "{display}: a {kind} inside the archive; searched as one binary file, not member by member"
            )),
            None => {}
        }
        // One level of compression is removed (the bomb guard) and this
        // member had two: `x.log.gz.gz`. Its lines were not read, only the
        // compressed bytes under the first level, so it is an error and
        // not a note: without it this was a 1 with no word.
        if let Some(kind) = v.compressed {
            unread.push(format!("{display}: still compressed ({kind}) after one level was removed: not searched"));
        }
        // Read to its end whatever the search found. `-q`, `-l`, `-m` and a
        // match after binary data all stop the *search* early, and stopping
        // the read there too made a cut or corrupt `.gz` a 0 under `-q` and
        // a 2 without it, and a 2 in a `.tgz` under either: "anything unread
        // is a 2, even when matches were found" has no exception for the
        // flag or the container. Only data with a checksum is read on (see
        // `Rest::Verify`), so `-q` on a plain file still stops at its match.
        Ok(crate::archive::Rest::Verify)
    });
    // A note, not an error: nothing is known to be missing, and the format
    // offers no way to find out. But a 1 is someone's "the logs are clean",
    // and this file cannot back it up.
    if let Some(format) = out.unverified_end
        && !matched
        && out.errors.is_empty()
    {
        notes.push(format!(
            "{}: no match, but {format} cannot detect truncation: a cut-off file reads as a shorter one",
            crate::source::display(path)
        ));
    }
    out.errors.extend(unread);
    out.matched = matched;
    out.leads_with_line = leads_with_line;
    out.kept = kept;
    out.stats = stats;
    out.notes = notes;
    out
}

/// `-T/--list-members`: print member paths without searching.
///
/// Deliberately does **not** short-circuit on the first member: the exit-code
/// contract applies here too, so the whole archive is walked and any failure
/// on the way lands in `Outcome.errors`.
///
/// Lists every regular member, including ones a *search* would skip as binary.
/// Listing is a question about the tar header; skipping a binary member is a
/// question about its content, which `-T` never reads.
///
/// Takes the same `ctx` as [`search_archive`], and opens through the same
/// [`crate::archive::walk_archive`], because `-T` walks exactly as
/// many bytes of exactly the same archives. Opening through the plain
/// `open_decoded` instead would make `-T` the one mode that leaves a sweep's
/// worth of archive pages in the cache —
/// precisely the pollution the cache-dropping work exists to prevent.
pub fn list_members(
    ctx: &SearchCtx<'_>,
    path: &Path,
    out: &mut dyn io::Write,
) -> crate::archive::Outcome {
    let (sep, globs) = (ctx.sep, ctx.globs);

    let mut listed = false;
    let mut outcome = crate::archive::walk_archive(path, ctx.drop_cache, globs, |member, _| {
        let Some(shown) = plain_name(member, path, ctx) else {
            return Ok(crate::archive::Rest::Leave);
        };
        listed = true;
        writeln!(out, "{}", ctx.shown(display_path(shown, member, sep)))?;
        Ok(crate::archive::Rest::Leave)
    });
    // `-T` has no notion of a match, so "listed something" is what stands in
    // for it: `trg -T ... ; echo $?` should say 1 when an archive held nothing
    // the globs would accept, the same shape of answer as a search.
    outcome.matched = listed;
    outcome.kept = listed;
    outcome
}
