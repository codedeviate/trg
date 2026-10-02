# Out of scope and wishlist

Two lists. **Out of scope** holds things `trg` deliberately does not do, each
with the reason — revisit one only when there is a *new* reason, not because it
looks like an obvious gap. **Wishlist** holds things that could reasonably be
added later.

When an item moves into the codebase, delete it here and record it in
[CHANGELOG.md](CHANGELOG.md).

## Out of scope

### Non-goals

- **Matching ripgrep's throughput.** Searching compressed logs is
  inflate-bound; the regex engine contributes almost nothing. Only the inflate
  implementation moves CPU.
- **Replacing `rg` for uncompressed source trees.** `trg` is for log archives.
- **Forking ripgrep or reimplementing its regex/search engine.** `trg` is a thin
  CLI over ripgrep's published crates.
- **Searching `$PWD` when no `PATH` is given.** An unset `$LOGS` would turn into
  a confident exit `1` from data nobody named. A missing `PATH` is a usage
  error (`2`).
- **A `-z` flag or filename-based format detection.** Compression is detected
  from content; extensions lie.
- **Parallelism inside a single gzip archive.** gzip has no index, so member N
  cannot be reached without inflating members 1..N-1. This is the format, not
  an implementation gap.

### Not built, for now

- **bzip2, xz, lz4.** None exist on the target servers and none are expected.
  Each would be mechanically simple (a sniff entry and a decoder) — the reason
  to add one is a real archive in that format.
- **`.zip` archives.** Not "another decoder": zip's central directory makes
  members individually seekable and skippable, which the streaming
  `for_each_member` model cannot express. Skipping is the only reason to want
  zip, so it needs a second traversal model. Reconsider if zip archives show up.

## Wishlist

### Features

- **`--archive-glob`** — filter archives (not members) during a directory walk.
- **Adaptive `--load-limit`** — a feedback loop that adjusts concurrency during
  the run, instead of a one-off check at startup.
- **zstd whole-buffer inflate** — zstd's `Frame_Content_Size` header is a more
  trustworthy size hint than gzip's mod-2³² trailer. Low priority: zstd already
  decompresses several times faster than gzip, so buffer mode gains least here.
- **Intra-archive parallelism for zstd** — frame boundaries make it possible,
  unlike gzip. It reopens `src/sched.rs`, which has produced four deadlocks;
  treat it as a project, not a patch.
- **A clearer `-c` signal for match-then-binary members** — stdout stays
  rg-compatible today and a stderr note breaks the silence, but a script that
  reads only stdout still has to check `$?`.

### Engineering

- **Deterministic ordering tests.** Four sleep-based tests in `tests/ordering.rs`
  use 150–500 ms margins and will flake on a loaded CI box. Redesign: block the
  straggler on a `Condvar` that `emit` signals, then `wait_timeout`.
- **Regression test for sched deadlock #4** (teardown guard armed after the
  spawn loop). Needs a fault-injection hook around `scope.spawn`.
- **x86-64 benchmarks.** Every figure so far is aarch64; zlib-ng's NEON and AVX2
  paths differ, so the backend comparison should be re-run before relying on it
  there.
