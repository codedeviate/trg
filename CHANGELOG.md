# Changelog

All notable changes to `trg` are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
How versions are chosen before 1.0.0 is described in
[CONTRIBUTING.md](CONTRIBUTING.md#versioning).

## [Unreleased]

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

[Unreleased]: https://github.com/codedeviate/trg/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/codedeviate/trg/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/codedeviate/trg/releases/tag/v0.1.0
