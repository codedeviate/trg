//! Terminal escaping. A log line is written by whoever sent the request, so it
//! can carry escape sequences that retitle the window, clear the screen, write
//! the clipboard, or on older terminals type text back into the shell.

use std::borrow::Cow;
use std::io::{self, Write};

use termcolor::{ColorSpec, HyperlinkSpec, WriteColor};

/// A writer that shows control bytes as text — `\x1b` rather than a real ESC —
/// so the terminal displays them instead of acting on them.
///
/// It sits between the printers and their `termcolor::Buffer`. trg's own
/// colours never pass through `write`: termcolor emits them from `set_color`
/// and `reset`, which are delegated untouched, so they keep working while every
/// byte that came out of a log or a member name is neutralised.
///
/// Escaped: C0 controls except tab and newline, DEL, the UTF-8 encoded C1
/// controls U+0080..U+009F (U+009B is a CSI on terminals that honour C1), a
/// raw byte 0x80..=0x9F that is not part of a UTF-8 character (the same CSI,
/// to a terminal in an 8-bit mode), and the bidirectional controls
/// U+202A..U+202E and U+2066..U+2069, which reorder what is displayed
/// around them. A carriage return is kept when it is half of a CRLF line
/// ending — escaping it would litter every line of a Windows log — and
/// escaped on its own, where it moves the cursor back over the line and can
/// hide what came before it.
///
/// A CR and its LF, and the bytes of one UTF-8 character, can straddle two
/// writes, so an unfinished one is held back until the rest arrives. Held
/// bytes survive a colour change — grep-printer resets colour between a
/// match ending in CR and its LF — and are settled on flush and on drop.
///
/// [`Escaped::json`] renders the same bytes as JSON escapes (`\u007f`), because
/// the JSON printer's output is already-encoded JSON: serde escapes C0 itself
/// but leaves DEL, C1 and the bidi controls raw inside strings, and those
/// are the only places such bytes can appear.
///
/// A literal `\x1b` already in a log prints the same as an escaped ESC. That
/// ambiguity is accepted: backslashes are not escaped, so Windows paths and
/// regexes in logs stay readable.
pub struct Escaped<W: Write> {
    inner: W,
    on: bool,
    json: bool,
    /// The start of a CRLF or of a UTF-8 character, waiting for its rest.
    held: [u8; 3],
    held_len: usize,
}

/// What to do with the bytes at the front of a slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Copy this many bytes as they are.
    Pass(usize),
    /// One byte, shown as `\xNN`.
    Byte,
    /// A character this many bytes long, shown by its code point.
    Char(usize, u32),
    /// Not decidable without the bytes after the end of the slice.
    More,
}

fn is_cont(b: u8) -> bool {
    (0x80..=0xBF).contains(&b)
}

/// Decide the front of `b`, which starts with a byte [`needs_look`] picked
/// out. `last` says nothing follows the slice.
///
/// A well-formed UTF-8 character is passed whole, so its continuation bytes
/// are never looked at on their own. One that is not well-formed gives up its
/// first byte only, and what follows is judged again from there: a
/// continuation byte met that way belongs to no character, and 0x80..=0x9F
/// among them are the raw C1 controls.
fn step(b: &[u8], last: bool) -> Step {
    let more = if last { Step::Pass(1) } else { Step::More };
    // The second byte's range is narrower after E0 and F0, which excludes
    // the overlong forms: no terminal should decode one, and passed whole it
    // would carry a C1 byte through as a continuation.
    let (len, lo) = match b[0] {
        b'\r' => {
            return match b.get(1) {
                Some(b'\n') => Step::Pass(1),
                Some(_) => Step::Byte,
                None if last => Step::Byte,
                None => Step::More,
            };
        }
        0x00..=0x1F | 0x7F | 0x80..=0x9F => return Step::Byte,
        0xC2..=0xDF => (2, 0x80),
        0xE0 => (3, 0xA0),
        0xE1..=0xEF => (3, 0x80),
        0xF0 => (4, 0x90),
        0xF1..=0xF4 => (4, 0x80),
        _ => return Step::Pass(1),
    };
    for i in 1..len {
        match b.get(i) {
            None => return more,
            Some(&c) if is_cont(c) && (i > 1 || c >= lo) => {}
            Some(_) => return Step::Pass(1),
        }
    }
    match (b[0], b[1], b.get(2)) {
        (0xC2, c @ 0x80..=0x9F, _) => Step::Char(2, c as u32),
        (0xE2, 0x80, Some(&c @ 0xAA..=0xAE)) => Step::Char(3, 0x2000 + (c as u32 - 0x80)),
        (0xE2, 0x81, Some(&c @ 0xA6..=0xA9)) => Step::Char(3, 0x2040 + (c as u32 - 0x80)),
        _ => Step::Pass(len),
    }
}

impl<W: Write> Escaped<W> {
    pub fn new(inner: W, on: bool) -> Self {
        Self { inner, on, json: false, held: [0; 3], held_len: 0 }
    }

    /// Switch an unused writer to JSON escapes.
    pub fn into_json(mut self) -> Self {
        self.json = true;
        self
    }

    pub fn json(inner: W, on: bool) -> Self {
        Self { inner, on, json: true, held: [0; 3], held_len: 0 }
    }

    /// One escaped byte, or one escaped character by its code point.
    fn emit(&mut self, value: u32, char: bool) -> io::Result<()> {
        match (self.json, char) {
            (true, _) => write!(self.inner, "\\u{value:04x}"),
            (false, true) => write!(self.inner, "\\u{{{value:x}}}"),
            (false, false) => write!(self.inner, "\\x{value:02x}"),
        }
    }

    /// Write the front of `b` as `step` decided it; how many bytes that was.
    fn put(&mut self, s: Step, b: &[u8]) -> io::Result<usize> {
        match s {
            Step::Pass(n) => self.inner.write_all(&b[..n]).map(|()| n),
            Step::Byte => self.emit(b[0] as u32, false).map(|()| 1),
            Step::Char(n, c) => self.emit(c, true).map(|()| n),
            Step::More => Ok(0),
        }
    }

    /// Decide the held bytes with the start of `buf` after them, or with
    /// nothing after them when `last`. Returns how much of `buf` went with
    /// them; whatever is still undecided stays held, and then that is all of
    /// `buf`.
    fn settle_with(&mut self, buf: &[u8], last: bool) -> io::Result<usize> {
        let mut used = 0;
        while self.held_len > 0 {
            let h = self.held_len;
            let take = (buf.len() - used).min(3);
            let mut joined = [0u8; 6];
            joined[..h].copy_from_slice(&self.held[..h]);
            joined[h..h + take].copy_from_slice(&buf[used..used + take]);
            let joined = &joined[..h + take];
            let s = step(joined, last);
            if s == Step::More {
                // Fewer than three bytes were left in `buf`, or the
                // character would have been complete.
                self.held[..joined.len()].copy_from_slice(joined);
                self.held_len = joined.len();
                return Ok(buf.len());
            }
            let n = self.put(s, joined)?;
            if n >= h {
                self.held_len = 0;
                used += n - h;
            } else {
                // The held bytes did not make a character: the first went
                // out alone and the rest are judged again.
                self.held.copy_within(n..h, 0);
                self.held_len = h - n;
            }
        }
        Ok(used)
    }

    /// Nothing more is coming: a lone CR is escaped, and the start of a
    /// character that never finished goes out as the bytes it is.
    fn settle(&mut self) -> io::Result<()> {
        self.settle_with(&[], true).map(|_| ())
    }
}

impl<W: Write> Write for Escaped<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.on {
            self.inner.write_all(buf)?;
            return Ok(buf.len());
        }
        if buf.is_empty() {
            return Ok(0);
        }

        let mut i = self.settle_with(buf, false)?;

        // Safe bytes are copied in runs; only an escape breaks a run. The scan
        // for the next byte worth a second look is a plain predicate so it
        // stays a tight loop: on a matching-heavy run nearly every byte is
        // safe, and a per-byte `match` tripled the CPU spent printing.
        let mut run = i;
        while i < buf.len() {
            match buf[i..].iter().position(|&b| needs_look(b)) {
                Some(n) => i += n,
                None => break,
            }
            match step(&buf[i..], false) {
                Step::Pass(n) => i += n,
                Step::More => {
                    self.inner.write_all(&buf[run..i])?;
                    let rest = &buf[i..];
                    self.held[..rest.len()].copy_from_slice(rest);
                    self.held_len = rest.len();
                    return Ok(buf.len());
                }
                s => {
                    self.inner.write_all(&buf[run..i])?;
                    i += self.put(s, &buf[i..])?;
                    run = i;
                }
            }
        }
        self.inner.write_all(&buf[run..])?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.settle()?;
        self.inner.flush()
    }
}

/// Bytes that might be escaped, or start a sequence that might be: the C0
/// controls but tab and newline, DEL, and everything that is not ASCII. What
/// becomes of one is [`step`]'s to say.
fn needs_look(b: u8) -> bool {
    (b < 0x20 && b != b'\t' && b != b'\n') || b >= 0x7F
}

impl<W: Write> Drop for Escaped<W> {
    fn drop(&mut self) {
        // Nowhere to report a failure from a drop; the bytes before it have
        // already gone through `write`, which did report.
        let _ = self.settle();
    }
}

impl<W: WriteColor> WriteColor for Escaped<W> {
    fn supports_color(&self) -> bool {
        self.inner.supports_color()
    }
    fn set_color(&mut self, spec: &ColorSpec) -> io::Result<()> {
        self.inner.set_color(spec)
    }
    fn reset(&mut self) -> io::Result<()> {
        self.inner.reset()
    }
    fn is_synchronous(&self) -> bool {
        self.inner.is_synchronous()
    }
    fn set_hyperlink(&mut self, link: &HyperlinkSpec) -> io::Result<()> {
        self.inner.set_hyperlink(link)
    }
    fn supports_hyperlinks(&self) -> bool {
        self.inner.supports_hyperlinks()
    }
}

/// Whether stderr's control bytes are escaped, and whether a diagnostic is
/// kept to one line.
static STDERR: std::sync::OnceLock<(bool, bool)> = std::sync::OnceLock::new();

/// Decide once, at startup, how stderr is escaped: control bytes (`on`), and
/// newlines inside one diagnostic (`one_line`). Process-wide because
/// diagnostics are printed from deep inside the archive reader, where
/// threading a flag through would touch every signature on the way.
pub fn set_stderr(on: bool, one_line: bool) {
    let _ = STDERR.set((on, one_line));
}

fn stderr_mode() -> (bool, bool) {
    *STDERR.get_or_init(|| (io::IsTerminal::is_terminal(&io::stderr()), true))
}

/// `text` for stderr, under the stderr decision, newlines kept: for a message
/// of trg's own that has several lines, as a usage error does. Before
/// `set_stderr` has run — a flag that failed to parse — it falls back to
/// `auto`.
pub fn for_stderr(s: &str) -> Cow<'_, str> {
    text(s, stderr_mode().0)
}

/// One diagnostic about an archive, a member or a path, as one line: names
/// come out of archives and directories, and tar's own error text quotes
/// header bytes, so a newline in one would start a line that trg never
/// wrote. See [`name`].
pub fn diagnostic(s: &str) -> Cow<'_, str> {
    let (on, one_line) = stderr_mode();
    match if one_line { name(s) } else { Cow::Borrowed(s) } {
        Cow::Borrowed(s) => text(s, on),
        Cow::Owned(s) => Cow::Owned(text(&s, on).into_owned()),
    }
}

/// Write `trg: <msg>` and a newline to stderr, and never mind whether it
/// arrived. `eprintln!` panics when the write fails, as it does once the
/// reader of `2>&1 | head -1` has gone, and the release build aborts on a
/// panic: a SIGABRT where the exit code should have been.
pub fn warn(msg: &str) {
    let _ = writeln!(io::stderr().lock(), "trg: {msg}");
}

/// A path or member name with its newlines and carriage returns shown as
/// `\x0a` and `\x0d`.
///
/// Apart from the terminal escaping above, and on in a pipe as well: a
/// member can be named `x\nother.tgz:auth.log:1:Accepted password for root`,
/// and every consumer of trg's output, a person or `cut -d:`, takes what
/// follows a newline for a line of its own. A matched line's newline is the
/// log's and stays; a name is one field of one line.
pub fn name(s: &str) -> Cow<'_, str> {
    if !s.contains(['\n', '\r']) {
        return Cow::Borrowed(s);
    }
    Cow::Owned(s.replace('\n', "\\x0a").replace('\r', "\\x0d"))
}

/// Whether [`text`] would leave `c` alone.
fn is_plain(c: char) -> bool {
    !matches!(c,
        '\0'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{7f}'..='\u{9f}'
        | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// [`Escaped`] for a diagnostic string bound for stderr, which carries paths
/// and member names and is a terminal just as often as stdout.
pub fn text(s: &str, on: bool) -> Cow<'_, str> {
    if !on || s.chars().all(is_plain) {
        return Cow::Borrowed(s);
    }
    let mut out = Vec::with_capacity(s.len() + 16);
    {
        let mut w = Escaped::new(&mut out, true);
        // A `Vec` never fails a write.
        let _ = w.write_all(s.as_bytes());
    }
    // Escaping only ever replaces whole characters with ASCII.
    Cow::Owned(String::from_utf8(out).expect("escaping keeps UTF-8 valid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `chunks` as separate writes, then drop, and return the output.
    fn esc(chunks: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut w = Escaped::new(&mut out, true);
            for c in chunks {
                w.write_all(c).unwrap();
            }
        }
        out
    }

    #[test]
    fn esc_and_bel_become_visible() {
        assert_eq!(esc(&[b"a\x1b]0;t\x07b"]), br"a\x1b]0;t\x07b");
    }

    #[test]
    fn newline_and_tab_pass_through() {
        assert_eq!(esc(&[b"a\tb\n"]), b"a\tb\n");
    }

    #[test]
    fn del_and_nul_are_escaped() {
        assert_eq!(esc(&[b"\x7f\x00"]), br"\x7f\x00");
    }

    #[test]
    fn crlf_split_across_writes_is_kept() {
        assert_eq!(esc(&[b"line\r", b"\n"]), b"line\r\n");
    }

    #[test]
    fn a_trailing_cr_at_the_very_end_is_escaped() {
        assert_eq!(esc(&[b"line\r"]), br"line\x0d");
    }

    #[test]
    fn a_c1_split_across_writes_is_escaped() {
        assert_eq!(esc(&[b"a\xc2", b"\x9bb"]), br"a\u{9b}b");
    }

    #[test]
    fn ordinary_utf8_split_across_writes_is_untouched() {
        // U+00E9 is C3 A9; U+00A0 is C2 A0, the byte after C2 but not a C1.
        assert_eq!(esc(&["é".as_bytes(), b"\xc2", b"\xa0"]), "é\u{a0}".as_bytes());
    }

    #[test]
    fn off_is_byte_exact() {
        let mut out = Vec::new();
        Escaped::new(&mut out, false).write_all(b"\x1b[2J\r").unwrap();
        assert_eq!(out, b"\x1b[2J\r");
    }

    /// grep-printer resets colour between a match ending in CR and the LF.
    #[test]
    fn crlf_survives_a_colour_reset_between_its_halves() {
        let mut buf = termcolor::Buffer::ansi();
        {
            let mut w = Escaped::new(&mut buf, true);
            w.set_color(ColorSpec::new().set_fg(Some(termcolor::Color::Red))).unwrap();
            w.write_all(b"crlf\r").unwrap();
            w.reset().unwrap();
            w.write_all(b"\n").unwrap();
        }
        let out = buf.into_inner();
        assert!(!out.windows(4).any(|w| w == br"\x0d"), "CRLF escaped: {out:?}");
        assert!(out.ends_with(b"\r\n"), "got: {out:?}");
    }

    /// JSON output is already escaped by serde except for DEL and C1, which it
    /// leaves raw; their escapes must be JSON's own or the output stops parsing.
    #[test]
    fn json_style_uses_json_escapes() {
        let mut out = Vec::new();
        {
            let mut w = Escaped::json(&mut out, true);
            w.write_all("{\"text\":\"a\u{7f}b\u{9b}c\"}\n".as_bytes()).unwrap();
        }
        assert_eq!(out, br#"{"text":"a\u007fb\u009bc"}"#.iter().copied().chain(*b"\n").collect::<Vec<_>>());
    }

    /// A lone 0x9B is a CSI to a terminal in an 8-bit mode. As the second
    /// byte of U+041B it is a letter.
    #[test]
    fn a_raw_c1_byte_is_escaped_and_a_continuation_byte_is_not() {
        assert_eq!(esc(&[b"a\x9b2Jb"]), br"a\x9b2Jb");
        assert_eq!(esc(&[b"\x80\x9f\xa0"]), b"\\x80\\x9f\xa0");
        let text = "Л é 日本 😀\n";
        assert_eq!(esc(&[text.as_bytes()]), text.as_bytes());
    }

    /// A lead byte with no character after it does not shelter the C1 byte
    /// that follows, and neither does an overlong form.
    #[test]
    fn a_c1_byte_after_a_broken_lead_is_escaped() {
        assert_eq!(esc(&[b"\xe2\x9bx"]), b"\xe2\\x9bx");
        assert_eq!(esc(&[b"\xe2", b"\x9b", b"x"]), b"\xe2\\x9bx");
        assert_eq!(esc(&[b"\xf0\x9f\x98x"]), b"\xf0\\x9f\\x98x");
        assert_eq!(esc(&[b"\xe0\x9b\x80"]), b"\xe0\\x9b\\x80");
        assert_eq!(esc(&[b"\xf0\x8f\xbf\xbf"]), b"\xf0\\x8f\xbf\xbf");
        assert_eq!(esc(&[b"\xe2\x9b"]), b"\xe2\\x9b", "settled at the end");
        // Latin-1 text: not UTF-8, and nothing in it is a control.
        assert_eq!(esc(&[b"caf\xe9 \xfcber\n"]), b"caf\xe9 \xfcber\n");
    }

    #[test]
    fn bidi_controls_are_escaped() {
        let s = "a\u{202a}\u{202e}b\u{2066}c\u{2069}d\u{2029}\u{2065}\u{206a}";
        let want = "a\\u{202a}\\u{202e}b\\u{2066}c\\u{2069}d\u{2029}\u{2065}\u{206a}";
        assert_eq!(esc(&[s.as_bytes()]), want.as_bytes());
        assert_eq!(text(s, true), want);
    }

    /// Every way of cutting the input in two, and one byte at a time, gives
    /// what one write gives.
    #[test]
    fn a_split_anywhere_changes_nothing() {
        let input = "a\r\nb\rc\u{9b}d\u{202e}e😀f\u{41b}\r".as_bytes().iter().copied()
            .chain(*b"\x9b\xe2\x80\xf0\x9f\x98\xe2\x80\xae\xc2\xc2\x9b\xe2\x9b\xf0\x9f")
            .collect::<Vec<u8>>();
        let whole = esc(&[&input]);
        for cut in 0..=input.len() {
            assert_eq!(esc(&[&input[..cut], &input[cut..]]), whole, "cut at {cut}");
        }
        let bytes: Vec<&[u8]> = input.chunks(1).collect();
        assert_eq!(esc(&bytes), whole);
    }

    #[test]
    fn json_style_escapes_a_bidi_control_as_json() {
        let mut out = Vec::new();
        {
            let mut w = Escaped::json(&mut out, true);
            w.write_all("\"a\u{202e}b\"".as_bytes()).unwrap();
        }
        assert_eq!(out, br#""a\u202eb""#);
    }

    #[test]
    fn a_name_keeps_to_one_line() {
        assert_eq!(name("a\nb\r\nc"), r"a\x0ab\x0d\x0ac");
        assert!(matches!(name("plain name.log"), Cow::Borrowed(_)));
    }

    #[test]
    fn text_escapes_a_str() {
        assert_eq!(text("a\x1b[31mb", true), r"a\x1b[31mb");
        assert_eq!(text("a\x1b[31mb", false), "a\x1b[31mb");
    }
}
