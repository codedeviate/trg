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
/// Escaped: C0 controls except tab and newline, DEL, and the UTF-8 encoded C1
/// controls U+0080..U+009F (U+009B is a CSI on terminals that honour C1). A
/// carriage return is kept when it is half of a CRLF line ending — escaping it
/// would litter every line of a Windows log — and escaped on its own, where it
/// moves the cursor back over the line and can hide what came before it.
///
/// Both of the two-byte decisions (CR then LF, C2 then a C1 byte) can straddle
/// two writes, so the first byte is held back until the next one arrives. A
/// held byte survives a colour change — grep-printer resets colour between a
/// match ending in CR and its LF — and is settled on flush and on drop.
///
/// [`Escaped::json`] renders the same bytes as JSON escapes (`\u007f`), because
/// the JSON printer's output is already-encoded JSON: serde escapes C0 itself
/// but leaves DEL and C1 raw inside strings, and those are the only places
/// such bytes can appear.
///
/// A literal `\x1b` already in a log prints the same as an escaped ESC. That
/// ambiguity is accepted: backslashes are not escaped, so Windows paths and
/// regexes in logs stay readable.
pub struct Escaped<W: Write> {
    inner: W,
    on: bool,
    json: bool,
    pending: Option<u8>,
}

impl<W: Write> Escaped<W> {
    pub fn new(inner: W, on: bool) -> Self {
        Self { inner, on, json: false, pending: None }
    }

    /// Switch an unused writer to JSON escapes.
    pub fn into_json(mut self) -> Self {
        self.json = true;
        self
    }

    pub fn json(inner: W, on: bool) -> Self {
        Self { inner, on, json: true, pending: None }
    }

    /// One escaped character: a single control byte, or the C1 code point
    /// whose second UTF-8 byte (`C2 xx`) is its value.
    fn emit(&mut self, value: u8, c1: bool) -> io::Result<()> {
        match (self.json, c1) {
            (true, _) => write!(self.inner, "\\u{value:04x}"),
            (false, true) => write!(self.inner, "\\u{{{value:x}}}"),
            (false, false) => write!(self.inner, "\\x{value:02x}"),
        }
    }

    /// Emit a held byte whose follower turned out not to be the one that
    /// changes its meaning: a lone CR is escaped, a lone C2 is ordinary UTF-8.
    fn settle(&mut self) -> io::Result<()> {
        match self.pending.take() {
            Some(b'\r') => self.emit(b'\r', false),
            Some(b) => self.inner.write_all(&[b]),
            None => Ok(()),
        }
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

        let mut i = 0;
        match (self.pending, buf[0]) {
            (Some(b'\r'), b'\n') => {
                self.pending = None;
                self.inner.write_all(b"\r")?;
            }
            (Some(0xC2), n @ 0x80..=0x9F) => {
                self.pending = None;
                self.emit(n, true)?;
                i = 1;
            }
            _ => self.settle()?,
        }

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
            let b = buf[i];
            let next = buf.get(i + 1).copied();
            let width = match (b, next) {
                (b'\t' | b'\n', _) => 0,
                (b'\r', Some(b'\n')) => 0,
                (b'\r' | 0xC2, None) => {
                    self.inner.write_all(&buf[run..i])?;
                    self.pending = Some(b);
                    return Ok(buf.len());
                }
                (0x00..=0x1F | 0x7F, _) => 1,
                (0xC2, Some(0x80..=0x9F)) => 2,
                _ => 0,
            };
            if width == 0 {
                i += 1;
                continue;
            }
            self.inner.write_all(&buf[run..i])?;
            if width == 1 {
                self.emit(b, false)?;
            } else {
                self.emit(buf[i + 1], true)?;
            }
            i += width;
            run = i;
        }
        self.inner.write_all(&buf[run..])?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.settle()?;
        self.inner.flush()
    }
}

/// Bytes that might be escaped, or start a pair that might be. CR and C2 are
/// only candidates: whether they are escaped depends on the byte after them.
fn needs_look(b: u8) -> bool {
    (b < 0x20 && b != b'\t' && b != b'\n') || b == 0x7F || b == 0xC2
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

static STDERR: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Decide once, at startup, whether stderr is escaped. Process-wide because
/// diagnostics are printed from deep inside the archive reader, where threading
/// a flag through would touch every signature on the way.
pub fn set_stderr(on: bool) {
    let _ = STDERR.set(on);
}

/// `text` for a diagnostic, under the stderr decision. Before `set_stderr` has
/// run — a flag that failed to parse — it falls back to `auto`.
pub fn for_stderr(s: &str) -> Cow<'_, str> {
    let on = *STDERR.get_or_init(|| io::IsTerminal::is_terminal(&io::stderr()));
    text(s, on)
}

/// [`Escaped`] for a diagnostic string bound for stderr, which carries paths
/// and member names and is a terminal just as often as stdout.
pub fn text(s: &str, on: bool) -> Cow<'_, str> {
    let clean = !s.bytes().any(|b| b < 0x20 && b != b'\t' && b != b'\n' || b == 0x7F)
        && !s.chars().any(|c| ('\u{80}'..='\u{9f}').contains(&c));
    if !on || clean {
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

    #[test]
    fn text_escapes_a_str() {
        assert_eq!(text("a\x1b[31mb", true), r"a\x1b[31mb");
        assert_eq!(text("a\x1b[31mb", false), "a\x1b[31mb");
    }
}
