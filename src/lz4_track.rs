//! Where an lz4 stream is in its frame structure, read off the compressed
//! bytes as they go past.
//!
//! lz4_flex treats the end of its input where a block size should be as the
//! end of the stream, whether or not the frame got its end mark and checksum
//! (`read_block` maps `UnexpectedEof` to `Ok(0)`). A `.lz4` cut between two
//! blocks therefore decoded cleanly to a shorter file, and a search of it was
//! an exit 1 about data never read. This reader passes every byte through
//! unchanged and only follows the framing, so the caller can ask, once the
//! input is exhausted, whether it ended between frames.

use std::io::{self, Read};

const FRAME: u32 = 0x184D_2204;
const LEGACY: u32 = 0x184C_2102;

fn is_skippable(m: u32) -> bool {
    m & 0xFFFF_FFF0 == 0x184D_2A50
}

#[derive(Debug, Clone, Copy)]
enum Then {
    /// The next block of a frame.
    Block,
    /// The next legacy block.
    Legacy,
    /// The next magic, after a skippable frame.
    Magic,
}

#[derive(Debug)]
enum State {
    /// Between frames: the next 4 bytes are a magic number.
    Magic,
    /// A skippable frame's 4-byte length.
    SkipLen,
    /// The frame descriptor after the magic, `need` bytes long once its
    /// first byte (FLG) says which optional fields are present.
    Header { need: usize },
    /// A block's 4-byte size, or 0 for the end mark.
    BlockSize,
    /// A legacy (`lz4 -l`) block size. Legacy frames have no end mark: they
    /// end at the end of the input or where a magic number takes a size's
    /// place, so this is also a place the input may end.
    LegacySize,
    /// Bytes to pass over: block data and checksums, a skippable frame's body.
    Skip(u64),
    /// The content checksum after the end mark, then back to `Magic`.
    ContentSum,
    /// Not lz4 framing as this reader knows it. lz4_flex reports that itself;
    /// this reader stops following.
    Lost,
}

pub struct Lz4Track<R> {
    inner: R,
    state: State,
    /// The bytes of a field being collected: a magic, a size, a header.
    field: Vec<u8>,
    /// Where a `Skip` leads once it is over.
    then: Then,
    block_sum: bool,
    content_sum: bool,
}

impl<R: Read> Lz4Track<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            state: State::Magic,
            field: Vec::with_capacity(16),
            then: Then::Magic,
            block_sum: false,
            content_sum: false,
        }
    }

    /// Whether the bytes seen so far end inside a frame: true for a stream
    /// that was cut off, once the input is exhausted.
    pub fn mid_frame(&self) -> bool {
        match self.state {
            State::Magic | State::LegacySize => !self.field.is_empty(),
            State::Lost => false,
            _ => true,
        }
    }

    fn follow(&mut self, mut b: &[u8]) {
        while !b.is_empty() {
            if let State::Skip(n) = self.state {
                let k = (n as usize).min(b.len());
                b = &b[k..];
                self.state = match n - k as u64 {
                    0 => self.after_skip(),
                    left => State::Skip(left),
                };
                continue;
            }
            if let State::Lost = self.state {
                return;
            }
            let want = match self.state {
                State::Header { need } => need,
                _ => 4,
            };
            let k = (want - self.field.len()).min(b.len());
            self.field.extend_from_slice(&b[..k]);
            b = &b[k..];
            if let State::Header { need } = self.state
                && self.field.len() == 1
            {
                let flg = self.field[0];
                self.block_sum = flg & 0x10 != 0;
                self.content_sum = flg & 0x04 != 0;
                let full = 3 + if flg & 0x08 != 0 { 8 } else { 0 } + if flg & 0x01 != 0 { 4 } else { 0 };
                if need != full {
                    self.state = State::Header { need: full };
                }
                continue;
            }
            if self.field.len() == want {
                self.state = self.field_done();
                self.field.clear();
            }
        }
    }

    fn after_skip(&self) -> State {
        match self.then {
            Then::Block => State::BlockSize,
            Then::Legacy => State::LegacySize,
            Then::Magic => State::Magic,
        }
    }

    fn field_done(&mut self) -> State {
        let word = || u32::from_le_bytes(self.field[..4].try_into().unwrap());
        match self.state {
            State::Magic => self.magic(word()),
            State::LegacySize => match word() {
                m if m == FRAME || m == LEGACY || is_skippable(m) => self.magic(m),
                n => self.skip(n as u64, Then::Legacy),
            },
            State::SkipLen => self.skip(word() as u64, Then::Magic),
            State::Header { .. } => State::BlockSize,
            State::BlockSize => match word() {
                0 if self.content_sum => State::ContentSum,
                0 => State::Magic,
                n => self.skip((n & 0x7FFF_FFFF) as u64 + if self.block_sum { 4 } else { 0 }, Then::Block),
            },
            State::ContentSum => State::Magic,
            State::Skip(_) | State::Lost => State::Lost,
        }
    }

    fn magic(&self, m: u32) -> State {
        match m {
            FRAME => State::Header { need: 1 },
            LEGACY => State::LegacySize,
            m if is_skippable(m) => State::SkipLen,
            _ => State::Lost,
        }
    }

    /// Pass over `n` bytes, then go on to `then`.
    fn skip(&mut self, n: u64, then: Then) -> State {
        self.then = then;
        if n == 0 { self.after_skip() } else { State::Skip(n) }
    }
}

impl<R: Read> Read for Lz4Track<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.follow(&buf[..n]);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(b: &[u8]) -> Lz4Track<&[u8]> {
        let mut t = Lz4Track::new(b);
        io::copy(&mut t, &mut io::sink()).unwrap();
        t
    }

    #[test]
    fn a_header_alone_is_mid_frame() {
        assert!(track(&[0x04, 0x22, 0x4d, 0x18, 0x64, 0x40, 0xa7]).mid_frame());
    }
}
