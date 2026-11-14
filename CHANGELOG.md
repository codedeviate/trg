# Changelog

All notable changes to `targrep` (`trg`) are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
How versions are chosen before 1.0.0 is described in
[CONTRIBUTING.md](CONTRIBUTING.md#versioning).

## [Unreleased]

### Security

- A tar's long-name, long-link and pax headers are read up to 1 MiB and
  refused beyond it, an exit 2. A 4.6 MB tgz declaring a 1 GiB name took
  1-4 GB of memory and printed the "name".
- A newline or carriage return in a file or member name is shown as `\x0a`
  or `\x0d`, piped or not, unless `--escape never`. A member named
  `x\nauth.log:1:Accepted password for root` printed a second line no
  reader could tell from a match. Diagnostics are kept to one line each.
- On a terminal, raw C1 bytes outside well-formed UTF-8 (a lone `0x9b` is
  CSI on an 8-bit terminal) and the bidi controls U+202A-202E and
  U+2066-2069 are shown as text.
- A log whose first line puts `ustar` at byte 257 is a log, not a tar: a
  tar is now known by its header checksum. Whoever sent the first request
  after rotation could make the day's log an exit 2 that searched nothing.

### Changed

- **Breaking:** `-q`, `-l`, `-m` and a match after binary data stop the
  search early but no longer stop the read: compressed data is read to its
  end, so a cut or corrupt `.gz`, `.xz`, `.bz2`, `.zst`, `.lz4` or zip
  member is a 2 in every mode, as it already was inside a `.tgz`. It was a
  0. `-q` on a large `.gz` now costs the decompression of the rest; a plain
  file still stops at its match.
- **Breaking:** files trg cannot read are an exit 2, `not searched`, where
  their bytes were searched as text for a silent 1: lzip, 7z, rar, cpio,
  lzop and lzma-alone, at the top level, under a compression, and as a
  member; and data still compressed after one level was removed
  (`x.log.gz.gz`). A sweep of a directory holding such files turns from 1
  to 2.
- **Breaking:** `-T` with `-q`, `-l`, `-c` or `--json` is a usage error;
  the flag was ignored.
- trg reads tars with its own reader, not the `tar` crate. Error texts
  under `tar entry:` changed.
- A zip entry named as a directory or flagged a symlink is searched and
  listed when it carries data.
- A stream ending in a legacy lz4 frame (`lz4 -l`) with no match gets the
  note `.Z` gets: the format cannot show a cut.
- `--nice` only ever lowers priority, and says so when it could not be set.

### Fixed

- Tar members under a typeflag other than `0` are searched and listed:
  sparse (`tar -S`, old GNU and all three pax forms, expanded to the
  original file under its real name), contiguous, unknown letters, links
  carrying data. They were skipped without a word, a 1.
- A tar cut at a header boundary after a member ending in 512 zero bytes
  or more is a 2. The member's own zeros passed for the end-of-archive
  blocks, a 1.
- v7 tars and tars behind leading zero blocks are read as tars, so their
  compressed members are opened.
- A zip behind another zip or a script (`cat a.zip b.zip`, a
  self-extractor) is a 2 naming the bytes not searched, and a file that
  ends as a zip is read as one. Only the last zip was read, a 1.
- A zip whose end record counts fewer entries than its directory holds, or
  with data no entry covers, is a 2. `unzip` saw entries trg did not.
- Concatenated legacy lz4 frames, and an lz4 frame behind a skippable frame
  longer than 512 bytes, are read. Both were a false 2.
- bsdtar's zero-padded `.tar.Z` (`tar -cZf - dir`) is no longer a false 2.
- `-v -m 0` prints nothing and is a 1, as in rg; it printed lines.
- The `--` between archives follows rg: none before a lone `binary file
  matches`, `--\r\n` under `--crlf`, none in `-T`.
- `-g 'logs/*'` matches a member stored as `/logs/x` or `../logs/x`.
- A closed stderr pipe no longer aborts trg (SIGABRT); a closed stdout, or
  a closed stdin named by `-`, is a 2 where output was lost for a 0.
- A plain file that shrank mid-read is a 2 even when it matched.

## [0.16.0] - 2026-11-07

### Changed

- **Breaking:** binary data is searched, not skipped at its first NUL byte,
  as rg searches a file named on its command line (`rg --binary` for one
  it finds). A NUL
  ends a line; a match prints `binary file matches` where its line would
  be, and `-a` prints the lines. A 1 now means every byte was read: a log
  reopened at its old offset after logrotate's `copytruncate` starts with a
  run of NULs, and its lines after them were never searched, a 1 even for
  `trg ERR app.log` where `rg ERR app.log` found the match. Binary data
  holding the pattern, a core dump say, is now a 0.
- `-c` counts a member that matched and then hit binary data, as `rg
  --binary -c` does, and the stderr note about a suppressed count is gone.
- A plain file found to be binary leaves the page cache, as an archive
  does, so a sweep of /var/log does not pull journals and core dumps into
  it. A text file stays cached: it may be a live log.
- `/dev/zero` and other endless streams are read until stopped, as by rg
  and grep, in flat memory.

### Fixed

- A match after binary data is found with `-B` or `-C`. The line before it
  came first, the printer refused it, and the search ended: a 1. rg 15.2
  has the same bug (`rg --binary -B1`).

## [0.15.2] - 2026-11-07

### Fixed

- A plain binary file over 64 KiB with no match is a 1 again, not a 2
  saying it "shrank while being read". Its search stops at the first NUL on
  purpose, and the check for a log truncated mid-read, new in 0.15.0, took
  the unread rest for a cut: `trg ERR /var/log/` was a 2 wherever wtmp,
  lastlog or a journal file sat. The check now runs only once a read met
  the end of the file, and only for a file now smaller than when it was
  opened: Linux's sysfs states 4096 bytes for a file of a few.
- An empty directory with `--archive-glob` says only that it is empty, not
  also that the glob matched nothing.

- Concatenated archives are searched whole: `cat day1.tgz day2.tgz >
  week.tgz` is one valid archive holding two tars, in every format, and the
  search stopped at the first tar's end. A match in the second was an exit
  1 about members never read. trg now reads on past the end-of-archive
  blocks, as `tar -i` does.
- A tar inside an archive gets the note a zip there gets, saying its
  members are not searched: `tar -cf month.tar day*.tgz` was a 1 with no
  word that the days inside were never read.
- A zip on a pipe (`cat x.zip | trg ERR -`) says it cannot be read from a
  pipe, where it said only "Illegal seek". Still an exit 2.
- `-g` ignores a leading `./` in member names. `tar -czf day.tgz -C
  /var/log .` names its members `./apache2/access.log`, and `-g
  'apache2/*'` matched none of them, so beside an archive named without it
  one was searched and the other silently skipped.
- Bytes after a tar's end that are neither zeros nor another tar are an
  exit 2. They were read and thrown away. A byte changed in a `.tar.Z`,
  which has no checksum, gave a silent 1 in 68 of 100 torture runs; now
  none do.

## [0.15.1] - 2026-11-07

### Fixed

- An encrypted zip entry is reported as `encrypted, and trg takes no
  password: not searched`. The message was stuffr's, and advised building
  it with zip's `aes-crypto` feature, which no build of trg would use.
- Zero padding after a legacy lz4 stream (`lz4 -l`) is padding whatever its
  length. A length that was not a multiple of four left a part of a block
  size over and was an exit 2, "cut off mid-frame".

## [0.15.0] - 2026-11-06

### Changed

- **Breaking:** a directory with no files in it is now an exit 2, with
  `dir: no files in this directory` on stderr. It searched nothing, and the
  1 it gave, as rg does, read as clean logs for an unmounted mount point or
  a log directory spelled wrong. A directory holding only FIFOs or sockets
  counts as empty.

### Added

- A `PATH` of `-` reads standard input, as in rg: `zcat x.gz | trg ERR -`,
  or a whole archive piped in. Its hits are named `<stdin>`. `-g` does not
  filter it.
- `--json` output ends with a `summary` record, as rg's does: the run's
  elapsed time and its statistics summed over every member searched.

### Fixed

- Zero bytes after the end of a gzip, bzip2, zstd or lz4 stream are read as
  padding. bsdtar, macOS's tar, writes `tar czf - dir > x.tgz` in 10 KiB
  blocks and fills the last with zeros, and every such archive was an exit 2
  ("after the tar's end: invalid gzip header") although tar reads it whole.
- Up to six bytes of junk after an lz4 stream are an exit 2. lz4_flex reads
  a tail shorter than a frame header as the end of the input, so it decoded
  cleanly, where `lz4 -d` reports an invalid frame.
- A plain log truncated in place while trg reads it (logrotate's
  `copytruncate`) is now an exit 2 saying it shrank. The read met the new
  end of the file early, and a log whose match was in the lost part was a 1.
  A pipe has no size to fall short of and is not checked.
- `-g` picks a plain file by any name it was reached by. A log and a symlink
  or hardlink to it are searched once, under the first name the walk met,
  and `-g` used to judge that name alone: `-g current.log` beside
  `access.log.2026-11-05` searched nothing.
- `-g` judges a plain file's path relative to the current directory, as rg
  does, so `logs`, `./logs` and an absolute path select the same files.
  `-g 'logs/*.log' ./logs` used to select none.
- A directory walk's errors name the path once:
  `t1/locked: Permission denied (os error 13)`, not
  `t1: t1/locked: IO error for operation on t1/locked: Permission denied`.
- A zip inside an archive now gets a note on stderr saying its entries are
  not searched. It read as binary and was skipped without a word, so a 1
  said nothing about the logs inside it.
- A member with a NUL byte in its first 64 KB is now skipped as binary in
  every format, as rg skips such a file. gzip and xz deliver data in short
  reads, so in a `.tgz`, `.tar.xz` or `.gz` the lines before the NUL were
  searched and printed, an exit 0, where the same member in a `.tar.zst`,
  `.zip` or plain `.tar` was a 1.

## [0.14.0] - 2026-11-05

### Changed

- **Breaking:** a `-g` that keeps no member in the whole run is now an exit
  2, with `trg: -g matched no members in the archives searched`, as an
  `--archive-glob` that matches nothing already was. A typo such as
  `-g acess.log` used to be a 1, which reads as "the logs are clean". An
  archive with no member past `-g` beside others that have one is still fine.
  `-T` follows the same rule.

## [0.13.1] - 2026-11-05

### Fixed

- With context on (`-A`, `-B`, `-C`), a `--` line now separates members and
  archives, as rg separates files. The last lines of one member no longer
  read as context for the first match in the next.
- `--load-limit` rejects `nan`, `inf` and negative values as usage errors
  (exit 2). `nan` never throttled.
- A bad number for `-m`, `-A`, `-B`, `-C`, `-j` or `--nice` names the option
  and gives the reason once: `--max-count expects a number, got '-1': invalid
  digit found in string`.

## [0.13.0] - 2026-11-02

### Added

- `bench/torture/`: the sweeps that found the fixes below (truncation,
  corruption, odd input, `-j`, signals, differential against rg, size limits
  and bombs) as scripts that fail on an unexpected result, on macOS and in a
  Linux container.

### Changed

- **Breaking:** a corrupt archive is now an exit 2 wherever its format can
  tell. trg stopped reading at the tar's end-of-archive blocks, so the gzip,
  bzip2 and lz4 checksums after them were never checked, and it stopped
  reading a member at its first NUL, short of that member's own checksum. A
  corrupt `.tgz` could come back a 0 with shifted line numbers, or a 1. In a
  sweep of one flipped byte at random places, 36% of `.tgz`, 24% of
  `.tar.bz2` and 15% of `.tar.lz4` cases were a silent 1 or a 0; now every
  one is a 2. A corrupt gzip trailer, documented as a 0, is a 2 too. Reading
  on stops where there is no checksum: a plain stream such as `/dev/zero`
  still stops at its first NUL. Found by a torture run.

### Fixed

- An `.lz4` cut off between two blocks decoded cleanly to a shorter file, a
  1 about data never read. lz4_flex takes the end of its input where a block
  size should be for the end of the stream; trg now follows the frame
  structure itself and makes that a 2.
- `-m` behaves as in rg: the after-context of the last allowed match is
  printed, `-v -m N` no longer prints an extra line when the last line has no
  newline, and `-m 0` prints nothing and exits 1 instead of printing a match.
- `-A` and `-B` win over `-C` whichever comes first, and the last of `-i` and
  `-S` wins, as in rg.
- `-c -o` counts matches, not matching lines, as in rg.
- A plain `.tar` cut off at a block boundary between two members read as a
  whole, shorter archive, a 1 about the members never read. A tar that does
  not end in zero blocks is now reported cut short, a 2.
- `-v -m N` with after-context printed every line after the limit, each as
  a match. grep-searcher 0.1.17 searches the rest of a buffer as if nothing
  matched once the limit is reached, which under `-v` makes every line a
  match; rg 15.2 does the same reading a pipe. trg now enforces the limit in
  its own sink for that one combination, and prints what rg would on a
  file: the next `-A` lines, matches as matches, then stops.
- On macOS a zip with many entries took seconds per thousand: `F_NOCACHE`
  sent every small seek-and-read to the disk, 20 s for 70,000 entries. Zips
  are now read through the page cache on macOS (0.3 s); Linux still drops
  their pages after the read.
- The matcher was built without its line terminator (`crlf(false)` after it
  cleared it), which kept every search on the searcher's slow line-by-line
  path. On the fast path, searches with few matches run faster: 2 GiB of
  empty lines took 37 s and take 0.3 s.

## [0.12.0] - 2026-10-31

### Added

- A `.Z` (Unix `compress`) member inside an archive, such as a log rotated
  with `compresscmd compress`, is decompressed and searched like a `.gz` one.
  It used to be reported unread, a 2. stuffr 0.23 made its `.Z` decoder read
  from a borrowed reader, which an archive member is.

### Security

- stuffr 0.18.2 → 0.23.0, which moves zip from 8.6 to 9.x and fixes
  GHSA-m665-v7pm-7grj and RUSTSEC-2026-0367, -0368 and -0369. The last three
  are in zip's extraction to disk, which trg never does. The first is "zip
  cloaking": the local headers name different files from the central
  directory, so tools disagree on what a zip holds. trg already read by the
  central directory, as `unzip` does. It now also reports the disagreement,
  so a cloaked zip is an exit 2.
- The `zip` dev-dependency (test fixtures only) is on 9.x too.

- `zstd` 0.13 → 0.14.3 (zstd-safe 8.2.0, zstd-sys 2.1.2), which fixes
  RUSTSEC-2026-0391 and RUSTSEC-2026-0395 to -0406 and -0410. None of them
  reached trg: it decodes only through `zstd::stream::read::Decoder::new`,
  and the advisories are in context reuse, `Seekable`, dictionaries,
  encoders, the writer decoder and the WebAssembly allocator. The upgrade
  takes them out of the build so advisory scanners stop flagging trg.

## [0.11.1] - 2026-10-28

### Added

- The release tarballs carry a `THIRD-PARTY.md` with the licence notices of
  everything compiled into the binary, as several of those licences require
  (Info-ZIP, bzip2, the BSD-licensed zstd, among others). It is generated at
  release time by `scripts/third-party.py` from `Cargo.lock`, and CI runs the
  script on every push, so a new dependency without a licence file fails
  there.

### Fixed

- Documentation that said more than trg does. `--turbo` raises the job count
  and the priority and nothing else: reads stay at idle I/O priority and
  archives still leave the page cache, which the README said it lifted too.
  `--help` now says that `-l` prints `archive:member` and what happens when
  ordered output backs up, instead of naming an internal "spill cap". The
  README's download example pointed at 0.5.0.

## [0.11.0] - 2026-10-28

### Added

- `--examples`: worked examples of every option, from a first search to
  scripts, recipes and busy-server settings, all against one example tree of
  archives. It is living documentation: a test builds that tree, runs every
  example and checks the exit code each one claims, and fails when an option
  in `--help` has no example.

### Fixed

- `-o` with an anchored pattern (`^`, `$`) printed the first line's match and
  then every later hit as the whole line. The matcher was built single-line,
  so `^` held only at the start of the buffer; it is now built as ripgrep
  builds it, and a differential test against `rg -o` covers it.

## [0.10.0] - 2026-10-26

### Added

- `.zip` archives, through the `stuffr-formats` crate's zip container. A
  zip's members are searched like a tar's: per-member line numbers, `-g`,
  `-T`, binary detection, and one level of compression inside a member
  (`access.log.1.gz` in a zip). Stored, deflate, deflate64, bzip2, LZMA, xz
  and PPMd members are read. An encrypted member, a damaged one (CRC
  mismatch), or records the archive cannot account for are reported and
  make the run a 2. The members after an encrypted one are still searched.
- Unix `compress` (`.Z`, `.tar.Z`), through stuffr's decoder. Only as a
  whole file: a `.Z` member inside an archive is reported unread (exit 2)
  rather than searched as binary.
  - `.Z` records neither its length nor a checksum, so a truncated one reads
    as a shorter file with no error. A `.Z` with no match is still a 1, but
    trg prints a note on stderr saying the file could not vouch for its own
    end.

### Fixed

- Errors from inside an archive did not say which archive: a sweep of a
  month of logs could print `trg: tar entry: unexpected end of file` and
  leave you to find it. Every such diagnostic now starts with the archive's
  path.

## [0.9.0] - 2026-10-25

### Added

- bzip2 and lz4, detected from content like the other formats: `.tar.bz2`,
  `.tar.lz4`, bare `.bz2` and `.lz4` logs, and `.bz2` or `.lz4` members inside
  any archive, one level deep. Both decoders are pure Rust, so the static
  Linux builds gain no C dependency.
  - bzip2 reads every stream in the file, so archives written by pbzip2 or
    lbzip2 (one stream per chunk) are read to the end.
  - lz4 reads every frame in the file (`cat a.lz4 b.lz4`), the legacy
    `lz4 -l` format, and steps over a leading skippable frame. A skippable
    frame between two data frames is reported as an error (exit 2).

## [0.8.0] - 2026-10-25

### Changed

- `--load-limit N` adapts during the run instead of checking once at
  startup. All `-j` workers start; while the 1-minute load average from other
  processes is above `N`, only one of them searches, and the rest wait between
  archives. The load is rechecked every 5 s, and one job is added back per
  check as it falls. trg's own workers are taken off the load before it is
  compared, so a wide `-j` does not throttle itself. Output, ordering and
  exit codes are unaffected.
- `--load-limit` now works on macOS too. It was Linux-only.

## [0.7.2] - 2026-10-24

### Fixed

- zstd archives made with a long-distance window larger than 128 MB
  (`zstd --long=28` up to `--long=31`) were refused with "Frame requires too
  much memory for decoding", exit 2, so they could not be searched at all.
  trg now accepts any window the format allows. Memory follows the data
  actually decoded, not the declared window: a `--long=31` archive of 91 MB
  peaked at 94 MB.

## [0.7.1] - 2026-10-24

### Fixed

- A zstd archive (or `.zst` member) that starts with a skippable frame — as
  `pzstd` output does — was taken for plain text. Its compressed bytes were
  searched as one binary file and the run exited 1, a claim of clean logs
  about data that was never decompressed. Skippable frames
  (`0x184D2A50`–`0x184D2A5F`) are now recognised as zstd.

## [0.7.0] - 2026-10-23

### Removed

- The whole-buffer inflate path (libdeflate) and the dependency on it. Since
  0.6.1 it was only used when asked for, and it never used less total CPU than
  zlib-ng streaming on any machine measured. trg always streams.
- `--inflate` and `--inflate-budget` no longer do anything. They are still
  accepted, and their values still checked (a typo is still exit 2), so
  scripts that pass them keep working; each prints a deprecation note on
  stderr. A later release will remove them.

### Changed

- `-g` globs without a `/` also match the member's file name, as in ripgrep
  and as `--archive-glob` already did: `-g access.log` now finds
  `logs/access.log`, where before it selected nothing. Matching the whole
  member path still applies too, so no glob selects fewer members than before.

## [0.6.1] - 2026-10-23

### Changed

- `--inflate=auto`, the default, now always streams. It used to decompress gzip
  archives that inflate to under `--inflate-budget` (64 MB) whole into memory,
  from a measurement that predates zlib-ng. Re-measured with
  `bench/inflate_modes.py` on x86-64, arm64 and Linux in a VM, that path never
  used less total CPU, and on archives inflating to 42 MB it used 8–59% more
  at 3.6× the memory. `--inflate=buffer` still opts in.

### Fixed

- A buffered inflate that cannot fit the budget is now refused before it
  starts. The gzip trailer is a lower bound on the output, and when it already
  exceeds `--inflate-budget` trg used to decompress the whole archive anyway,
  find it did not fit, and then stream it all again — twice the work for the
  same answer.

## [0.6.0] - 2026-10-23

### Added

- GitHub releases carry prebuilt Linux binaries (x86_64 and aarch64, glibc
  2.17 or newer) with SHA-256 checksums, built and smoke-tested by
  `.github/workflows/release.yml` on each tag. 0.5.0 was backfilled.
- x86-64 benchmark figures in the README: 9.4× less CPU than `zgrep -a`.
- Exclusion globs, as in ripgrep: a leading `!` excludes, for both `-g` and
  `--archive-glob` (`-g '!*.error.log'`, `--archive-glob '!2026-09-*'`). When
  several globs match, the last one given wins. With at least one non-`!`
  glob the list is a whitelist; with only `!` globs, everything else is
  searched.

### Changed

- A glob starting with `!` used to be matched literally, so it matched
  nothing; it now excludes.
- `rust-version = "1.88"` is declared (the floor set by `globset` and
  `ignore`), so an older toolchain gets a clear error instead of a
  dependency failure. CI checks it.

## [0.5.0] - 2026-10-23

### Added

- `--archive-glob PAT`: when walking a directory, search only the archives
  matching `PAT`; repeatable, and an archive matching any of them is searched.
  A glob without `/` matches the file name at any depth (`2026-08-*.tgz`); one
  with `/` matches the path below the directory named (`web1/*.tgz`). Files
  named on the command line are always searched, as with `rg -g`.
- An `--archive-glob` that matches no file anywhere in the walk is reported on
  stderr and exits 2, so a typo can never read as clean logs.

### Changed

- The project now has CI (tests and `clippy -D warnings` on Linux and macOS,
  plus the `--no-default-features` build), and the scheduler ordering tests no
  longer depend on timing.

## [0.4.0] - 2026-10-23

### Security

- Control bytes from logs and member names are no longer sent raw to a
  terminal. A log line is attacker-controlled (a request URL, a User-Agent),
  and its escape sequences could retitle the window, clear the screen, write
  the clipboard, hide earlier text behind a carriage return, or on vulnerable
  terminals type text back into the shell. On a tty they are now shown as
  text: `\x1b`, `\x07`, a lone CR as `\x0d`, UTF-8 C1 controls as `\u{9b}`.
  Tab, newline and CRLF line endings are kept. trg's own colours are
  unaffected.
- `--json` output gets JSON's own escapes instead (`\u007f`, `\u009b`) for
  the DEL and C1 bytes serde leaves raw, so it stays valid JSON either way.
- Diagnostics on stderr are escaped the same way, decided separately by
  whether stderr is a tty.

### Added

- `--escape WHEN` (`auto` | `always` | `never`, default `auto`): `auto`
  escapes only on a tty, so piped and redirected output is byte-exact, as
  before.

### Changed

- Output on a terminal can differ from `rg`'s for lines with control bytes.
  Piped output is unchanged.

## [0.3.1] - 2026-10-23

### Changed

- Published to crates.io as `targrep` (`cargo install targrep`); the binary is
  still `trg`. The Cargo package was renamed from `trg` to match; the library
  and binary names are unchanged.

### Documentation

- README: install via Homebrew (`codedeviate/cli/targrep`) or crates.io.
- CONTRIBUTING: crates.io and Homebrew steps added to the release checklist.

## [0.3.0] - 2026-10-23

### Added

- `scripts/build-linux.sh`: cross-builds a Linux release binary (x86_64 by
  default, or aarch64) into `dist/`, for copying onto servers by hand.
- xz support: `.tar.xz` archives search exactly like `.tgz` and `.tar.zst`,
  detected from content (magic `FD 37 7A 58 5A 00`), with no new flag.
- xz members inside an archive are unwrapped one level, so a logrotate'd
  `access.log.1.xz` is searched as text. Concatenated xz streams are decoded
  whole, as `xz -d` does.
- xz archives keep the bounded-memory streaming path and the page-cache
  dropping guarantee. `--inflate=buffer` falls back to streaming for them, as
  for zstd.
- liblzma is linked statically (built from source by `liblzma-sys`), so the
  Linux release binary still needs nothing but glibc.

## [0.2.1] - 2026-10-05

### Documentation

- Added `README.md`.

### Changed

- Licensed under MIT.

## [0.2.0] - 2026-08-25

Shipped as merge `8da74d6`. The version number, tag and this changelog were
applied afterwards, on 2026-10-02.

### Added

- zstd support: `.tar.zst` archives search exactly like `.tgz`. Compression is
  still detected from content (magic `28 B5 2F FD`), so there is no new flag.
- zstd members inside an archive are unwrapped one level, so a logrotate'd
  `access.log.1.zst` is searched as text instead of being skipped as binary.
- zstd archives keep the bounded-memory streaming path and the page-cache
  dropping guarantee (measured 19 MB → 0 MB resident).

### Changed

- `--inflate=buffer` is now documented as gzip-only: any other format always
  streams, which is a slower path and never a wrong answer.

### Documentation

- Added `CHANGELOG.md`, `CONTRIBUTING.md` (versioning and commit conventions)
  and `OUT-OF-SCOPE.md` (non-goals and wishlist).

## [0.1.0] - 2026-08-25

First release: tar-aware search of compressed log archives. Shipped as merge
`b47a8b4`.

### Added

- Search inside `.tgz` archives, reporting `archive.tgz:member/path:line:` with
  line numbers correct per member.
- Content-based format sniffing (gzip / tar / plain); no `-z` flag. Gzip members
  inside an archive are unwrapped one level, so `access.log.1.gz` is searched
  as text.
- Per-member binary detection, so `-a` is not needed for ordinary text logs.
- ripgrep-compatible flags wherever the job is the same: `-e`, `-i`, `-S`,
  `-w`, `-F`, `-v`, `-a`, `-m`, `--crlf`, `-n`/`-N`, `-A`/`-B`/`-C`, `-o`,
  `-c`, `-l`, `-q`, `--json`, `--color`.
- Archive options: `-g` filters *members* inside archives (a deliberate
  divergence from rg), `-T`/`--list-members`, `--archive-sep`, `--no-sort`.
- Directory walking that follows symlinks and de-duplicates by `(dev, ino)`, so
  a file reachable twice is searched once.
- Bounded archive-level worker pool (`-j`, default 1) with output in input
  order, producer-side backpressure, and panic-safe workers.
- Polite resource defaults: `--nice 10`, idle I/O priority, and page-cache
  dropping (`posix_fadvise(FADV_DONTNEED)`) so a search leaves 0 MB resident.
  Overrides: `--no-nice`, `--nice`, `--no-drop-cache`, `--turbo`,
  `--load-limit`.
- Inflate strategy `--inflate auto|stream|buffer` with a hard
  `--inflate-budget` ceiling and safe fallback to streaming. zlib-ng is the
  default backend; `--no-default-features` builds pure-Rust miniz_oxide.
- Exit-code contract: `0` matches and everything read, `1` no matches and
  everything read, `2` something went unread. A run given no `PATH` is a `2`,
  never a confident `1` from data nobody named.
- Benchmark harness in `bench/` against `zgrep -a` and `rg -za`.

### Performance

- On 610 MB compressed / 11.5 GB raw of real Apache logs (Linux aarch64):
  `trg -j1` uses 3.81 s CPU against `zgrep -a`'s 31.84 s — 8.4× less — and
  leaves 0 MB of page cache where the other tools leave 639 MB.

[Unreleased]: https://github.com/codedeviate/trg/compare/v0.16.0...HEAD
[0.16.0]: https://github.com/codedeviate/trg/compare/v0.15.2...v0.16.0
[0.15.2]: https://github.com/codedeviate/trg/compare/v0.15.1...v0.15.2
[0.15.1]: https://github.com/codedeviate/trg/compare/v0.15.0...v0.15.1
[0.15.0]: https://github.com/codedeviate/trg/compare/v0.14.0...v0.15.0
[0.14.0]: https://github.com/codedeviate/trg/compare/v0.13.1...v0.14.0
[0.13.1]: https://github.com/codedeviate/trg/compare/v0.13.0...v0.13.1
[0.13.0]: https://github.com/codedeviate/trg/compare/v0.12.0...v0.13.0
[0.12.0]: https://github.com/codedeviate/trg/compare/v0.11.1...v0.12.0
[0.11.1]: https://github.com/codedeviate/trg/compare/v0.11.0...v0.11.1
[0.11.0]: https://github.com/codedeviate/trg/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/codedeviate/trg/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/codedeviate/trg/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/codedeviate/trg/compare/v0.7.2...v0.8.0
[0.7.2]: https://github.com/codedeviate/trg/compare/v0.7.1...v0.7.2
[0.7.1]: https://github.com/codedeviate/trg/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/codedeviate/trg/compare/v0.6.1...v0.7.0
[0.6.1]: https://github.com/codedeviate/trg/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/codedeviate/trg/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/codedeviate/trg/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/codedeviate/trg/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/codedeviate/trg/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/codedeviate/trg/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/codedeviate/trg/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/codedeviate/trg/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/codedeviate/trg/releases/tag/v0.1.0
