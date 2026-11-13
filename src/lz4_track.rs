//! Where an lz4 stream is in its frame structure, read off the compressed
//! bytes as they go past.
//!
//! lz4_flex treats the end of its input where a block size should be as the
//! end of the stream, whether or not the frame got its end mark and checksum
//! (`read_block` maps `UnexpectedEof` to `Ok(0)`). A `.lz4` cut between two
//! blocks therefore decoded cleanly to a shorter file, and a search of it was
//! an exit 1 about data never read. This reader passes every byte through
//! unchanged and only follows the framing, so the caller can ask, once the
//! input is exhausted, whether it ended between frames. Reported upstream
//! as <https://github.com/PSeitz/lz4_flex/issues/246>.
//!
//! One thing it does withhold. A legacy (`lz4 -l`) frame has no end mark: it
//! ends where the input does, or where a magic number stands in a block
//! size's place, as in `cat a.lz4 b.lz4`. lz4_flex reads that magic as the
//! size of a block too big and fails, where `lz4 -d` reads on. So the input
//! is ended there instead ([`Lz4Track::at_frame_start`]), for the caller to
//! start a fresh decoder on what follows ([`Lz4Track::resume`]). To end it
//! exactly there, reads are cut to the framing: a field, or a block's data,
//! at a time.

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
    /// Zero bytes between frames: block padding, as bsdtar writes.
    Padding,
    /// Not lz4 framing as this reader knows it. lz4_flex reports that itself
    /// only when there are 7 bytes or more of it, enough for a frame header,
    /// so a shorter tail is the caller's to report: see [`Lz4Track::lost`].
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
    /// The last legacy block size read was 0. lz4 writes no such block, so
    /// it is zero padding after the stream, which legacy frames, with no end
    /// mark, read as a run of empty blocks: 3,000 zeros read cleanly, and
    /// 3,001 left one zero byte over and were "cut off mid-frame".
    legacy_padding: bool,
    /// A legacy block size being read, kept from the caller until all four
    /// bytes say whether it is a size or the next frame's magic.
    held: Vec<u8>,
    /// Bytes read from `inner` and followed, for the caller to take next.
    ready: Vec<u8>,
    /// `held` is a magic: the input reads as ended until [`Self::resume`].
    frame_start: bool,
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
            legacy_padding: false,
            held: Vec::with_capacity(4),
            ready: Vec::with_capacity(4),
            frame_start: false,
        }
    }

    /// Whether the input reads as ended only because the frame after a
    /// legacy one starts here.
    pub fn at_frame_start(&self) -> bool {
        self.frame_start
    }

    /// Go on reading, from the magic that ended the legacy frame.
    pub fn resume(&mut self) {
        if std::mem::take(&mut self.frame_start) {
            let magic = std::mem::take(&mut self.held);
            self.follow(&magic);
            self.ready = magic;
        }
    }

    /// Whether the bytes seen so far end in a legacy frame, which has no end
    /// mark and no checksum: cut between two blocks, it is a shorter stream
    /// and nothing can tell.
    pub fn ends_legacy(&self) -> bool {
        matches!(self.state, State::LegacySize)
    }

    /// Whether the bytes seen so far end inside a frame: true for a stream
    /// that was cut off, once the input is exhausted.
    pub fn mid_frame(&self) -> bool {
        match self.state {
            State::Magic => !self.field.is_empty(),
            State::LegacySize => {
                !self.field.is_empty() && !(self.legacy_padding && self.field.iter().all(|&b| b == 0))
            }
            State::Lost | State::Padding => false,
            _ => true,
        }
    }

    /// Whether bytes that are not lz4 framing followed the stream. `junk`
    /// after a frame decoded cleanly, and a search of it was a 1, where
    /// `lz4 -d` calls it an invalid frame.
    pub fn lost(&self) -> bool {
        matches!(self.state, State::Lost)
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
            if matches!(self.state, State::Magic | State::Padding) && self.field.is_empty() {
                let z = b.iter().take_while(|&&x| x == 0).count();
                b = &b[z..];
                if z > 0 {
                    self.state = State::Padding;
                }
                if b.is_empty() {
                    continue;
                }
                self.state = State::Magic;
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
                n => {
                    self.legacy_padding = n == 0;
                    self.skip(n as u64, Then::Legacy)
                }
            },
            State::SkipLen => self.skip(word() as u64, Then::Magic),
            State::Header { .. } => State::BlockSize,
            State::BlockSize => match word() {
                0 if self.content_sum => State::ContentSum,
                0 => State::Magic,
                n => self.skip((n & 0x7FFF_FFFF) as u64 + if self.block_sum { 4 } else { 0 }, Then::Block),
            },
            State::ContentSum => State::Magic,
            State::Skip(_) | State::Padding | State::Lost => State::Lost,
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
        loop {
            if buf.is_empty() || self.frame_start {
                return Ok(0);
            }
            if !self.ready.is_empty() {
                let n = self.ready.len().min(buf.len());
                buf[..n].copy_from_slice(&self.ready[..n]);
                self.ready.drain(..n);
                return Ok(n);
            }
            let want = match self.state {
                // With part of a size in `field`, the input ended inside it.
                State::LegacySize if self.field.is_empty() => {
                    let mut word = [0u8; 4];
                    let n = self.inner.read(&mut word[..4 - self.held.len()])?;
                    self.held.extend_from_slice(&word[..n]);
                    if n > 0 && self.held.len() < 4 {
                        continue;
                    }
                    if self.held.is_empty() {
                        return Ok(0);
                    }
                    let magic = <[u8; 4]>::try_from(&self.held[..])
                        .map(u32::from_le_bytes)
                        .is_ok_and(|m| m == FRAME || m == LEGACY || is_skippable(m));
                    if magic {
                        self.state = State::Magic;
                        self.frame_start = true;
                        return Ok(0);
                    }
                    // A size, or the few bytes of one the input ends in.
                    let size = std::mem::take(&mut self.held);
                    self.follow(&size);
                    self.ready = size;
                    continue;
                }
                State::Skip(n) => n.min(buf.len() as u64) as usize,
                State::Header { need } => need - self.field.len(),
                State::Lost => buf.len(),
                _ => 4 - self.field.len(),
            };
            let want = want.min(buf.len());
            let n = self.inner.read(&mut buf[..want])?;
            self.follow(&buf[..n]);
            return Ok(n);
        }
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
    fn zeros_between_frames_are_padding_and_other_bytes_are_lost() {
        let end = [0x04, 0x22, 0x4d, 0x18, 0x40, 0x40, 0xc0, 0, 0, 0, 0];
        let mut padded = end.to_vec();
        padded.extend([0; 9]);
        let t = track(&padded);
        assert!(!t.mid_frame() && !t.lost());
        padded.extend(b"junk");
        assert!(track(&padded).lost());
    }

    /// A legacy stream, then zeros of any length: padding. One to three
    /// bytes of a size after a real block is still a cut.
    #[test]
    fn zeros_after_a_legacy_stream_are_padding_whatever_their_length() {
        let mut legacy = vec![0x02, 0x21, 0x4c, 0x18, 3, 0, 0, 0, 0x20, b'x', b'\n'];
        assert!(!track(&legacy).mid_frame());
        let mut cut = legacy.clone();
        cut.push(0);
        assert!(track(&cut).mid_frame(), "a size cut after a real block");
        for pad in [4, 5, 6, 7, 3001] {
            let mut p = legacy.clone();
            p.extend(std::iter::repeat_n(0u8, pad));
            assert!(!track(&p).mid_frame(), "{pad} zeros");
        }
        legacy.extend([0, 0, 0, 0, 1]);
        assert!(track(&legacy).mid_frame(), "a non-zero byte after the padding");
    }

    /// The magic after a legacy frame is where the input is ended, with
    /// nothing of it handed over until the caller resumes.
    #[test]
    fn a_magic_after_a_legacy_frame_ends_the_input_until_resumed() {
        let one = [0x02, 0x21, 0x4c, 0x18, 3, 0, 0, 0, 0x20, b'x', b'\n'];
        let mut two = one.to_vec();
        two.extend(one);
        let mut t = Lz4Track::new(&two[..]);
        let mut got = Vec::new();
        io::copy(&mut t, &mut got).unwrap();
        assert_eq!(got, one);
        assert!(t.at_frame_start() && !t.mid_frame() && !t.lost());
        t.resume();
        io::copy(&mut t, &mut got).unwrap();
        assert_eq!(got, two);
        assert!(!t.at_frame_start() && !t.mid_frame() && t.ends_legacy());
    }

    /// Every byte still comes through, whatever the framing and however
    /// small the reads.
    #[test]
    fn every_byte_is_passed_through() {
        let mut b = vec![0x04, 0x22, 0x4d, 0x18, 0x40, 0x40, 0xc0, 5, 0, 0, 0x80, 1, 2, 3, 4, 5, 0, 0, 0, 0];
        b.extend([0; 7]);
        b.extend([0x02, 0x21, 0x4c, 0x18, 3, 0, 0, 0, 0x20, b'x', b'\n', 9, 9]);
        for step in [1, 2, 3, 5, 64] {
            let mut t = Lz4Track::new(&b[..]);
            let mut got = Vec::new();
            let mut buf = vec![0u8; step];
            loop {
                match t.read(&mut buf).unwrap() {
                    0 => break,
                    n => got.extend_from_slice(&buf[..n]),
                }
            }
            assert_eq!(got, b, "reads of {step}");
            assert!(t.mid_frame(), "ends in two bytes of a size");
        }
    }

    #[test]
    fn a_header_alone_is_mid_frame() {
        assert!(track(&[0x04, 0x22, 0x4d, 0x18, 0x64, 0x40, 0xa7]).mid_frame());
    }
}
