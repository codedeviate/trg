//! `--spaced`: an empty line after every row of the standard printer's output,
//! so a long line that wraps in the terminal is easy to tell from the next row.

use std::io::{self, Write};

use termcolor::{ColorSpec, HyperlinkSpec, WriteColor};

/// A writer that follows every line with an empty one.
///
/// It sits under [`Escaped`](crate::escape::Escaped), between it and the
/// buffer: escaping turns every control byte but tab and newline into text, and
/// a path or member name carries no raw newline, so each `\n` that reaches
/// this writer ends a row of output and nothing else.
///
/// The gap copies the row's own line ending, `\r\n` after a CRLF row and `\n`
/// after an LF one, so a Windows log does not come out with mixed endings. The
/// `--` that grep-printer writes between non-adjacent context groups already is
/// a blank-looking break, and gets no gap of its own.
///
/// A row can arrive in many small writes, so the writer remembers the length of
/// the current line, its first three bytes and its last one. That is enough to
/// tell `--`, `--\r` and anything else apart when the `\n` comes.
pub struct Spaced<W: Write> {
    inner: W,
    on: bool,
    /// Bytes of the current line so far.
    len: usize,
    /// The first three of them.
    head: [u8; 3],
    /// The latest one, to see whether the line ends in CR.
    last: u8,
}

impl<W: Write> Spaced<W> {
    pub fn new(inner: W, on: bool) -> Self {
        Self { inner, on, len: 0, head: [0; 3], last: 0 }
    }

    /// Note `bytes`, which hold no newline, as part of the current line.
    fn track(&mut self, bytes: &[u8]) {
        let Some(&last) = bytes.last() else { return };
        for (i, &b) in bytes.iter().enumerate().take(3usize.saturating_sub(self.len)) {
            self.head[self.len + i] = b;
        }
        self.len += bytes.len();
        self.last = last;
    }
}

impl<W: Write> Write for Spaced<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.on {
            self.inner.write_all(buf)?;
            return Ok(buf.len());
        }
        let mut rest = buf;
        while let Some(nl) = rest.iter().position(|&b| b == b'\n') {
            let (line, tail) = rest.split_at(nl + 1);
            self.inner.write_all(line)?;
            self.track(&line[..nl]);
            let crlf = self.last == b'\r';
            let separator = match self.len {
                2 => self.head[..2] == *b"--",
                3 => self.head == *b"--\r" && crlf,
                _ => false,
            };
            if !separator {
                self.inner.write_all(if crlf { b"\r\n" } else { b"\n" })?;
            }
            self.len = 0;
            self.last = 0;
            rest = tail;
        }
        self.inner.write_all(rest)?;
        self.track(rest);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: WriteColor> WriteColor for Spaced<W> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn through(on: bool, parts: &[&[u8]]) -> Vec<u8> {
        let mut w = Spaced::new(Vec::new(), on);
        for p in parts {
            w.write_all(p).unwrap();
        }
        w.inner
    }

    #[test]
    fn off_passes_bytes_untouched() {
        assert_eq!(through(false, &[b"a:1:x\n--\nb:2:y\n"]), b"a:1:x\n--\nb:2:y\n");
    }

    #[test]
    fn every_row_gets_an_empty_line_after_it() {
        assert_eq!(through(true, &[b"a:1:x\nb:2:y\n"]), b"a:1:x\n\nb:2:y\n\n");
    }

    #[test]
    fn a_row_written_in_pieces_gets_one_gap_at_its_end() {
        assert_eq!(through(true, &[b"a", b":", b"1", b":", b"x", b"\n"]), b"a:1:x\n\n");
    }

    #[test]
    fn the_separator_gets_no_gap_however_it_arrives() {
        assert_eq!(through(true, &[b"x\n", b"--\n", b"y\n"]), b"x\n\n--\ny\n\n");
        assert_eq!(through(true, &[b"x\n", b"-", b"-", b"\n", b"y\n"]), b"x\n\n--\ny\n\n");
    }

    #[test]
    fn crlf_rows_get_a_crlf_gap_and_the_crlf_separator_none() {
        assert_eq!(through(true, &[b"x\r\n--\r\ny\r\n"]), b"x\r\n\r\n--\r\ny\r\n\r\n");
    }

    #[test]
    fn a_row_that_only_starts_with_dashes_is_a_row() {
        assert_eq!(through(true, &[b"--x\n---\n"]), b"--x\n\n---\n\n");
    }
}
