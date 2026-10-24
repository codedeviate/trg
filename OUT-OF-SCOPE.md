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
- **A stdout signal for a `-c` member that matched and then hit binary data.**
  The count is genuinely unknown. `rg -c` over a directory prints nothing for
  such a file too (and exits 1; trg exits 0), and archive members are searched
  the way rg searches files it discovers, not files it is handed. Any marker on
  stdout breaks `-c`'s rg-compatible `member:count` format for
  every script that parses it. The stderr note (`binary data; count
  suppressed`) and the exit code already carry it, and both cost a pipeline
  nothing. Decided 2026-10-24.

### Not built, for now

- **bzip2, lz4.** None exist on the target servers and none are expected.
  Each would be mechanically simple (a sniff entry and a decoder) — the reason
  to add one is a real archive in that format.
- **`.zip` archives.** Not "another decoder": zip's central directory makes
  members individually seekable and skippable, which the streaming
  `for_each_member` model cannot express. Skipping is the only reason to want
  zip, so it needs a second traversal model. Reconsider if zip archives show up.

## Wishlist

### Features

- **Adaptive `--load-limit`** — a feedback loop that adjusts concurrency during
  the run, instead of a one-off check at startup. Waits for a reason: the
  default is `-j1` at `nice 10` with idle I/O, which already yields to Apache,
  so this only matters for `-j`/`--turbo` runs during sustained load.
- **Intra-archive parallelism for zstd** — frame boundaries make it possible,
  unlike gzip. It reopens `src/sched.rs`, which has produced four deadlocks;
  treat it as a project, not a patch. Waits for zstd archives on the servers:
  today they are gzip.
