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
- **Parallelism inside a single zstd archive.** Only possible across zstd
  frames, and the servers' archives (zstd since 2026-10, `tar --zstd`
  defaults) are one frame: `zstd -lv` on a production archive reports
  `Zstandard Frames: 1`. Plain `zstd`, `zstd -T0` and `tar --zstd` all write a
  single frame; only multi-frame tools such as `pzstd` would make this
  possible. Reconsider if the archiving changes to one of those.
- **A stdout signal for a `-c` member that matched and then hit binary data.**
  The count is genuinely unknown. `rg -c` over a directory prints nothing for
  such a file too (and exits 1; trg exits 0), and archive members are searched
  the way rg searches files it discovers, not files it is handed. Any marker on
  stdout breaks `-c`'s rg-compatible `member:count` format for
  every script that parses it. The stderr note (`binary data; count
  suppressed`) and the exit code already carry it, and both cost a pipeline
  nothing. Decided 2026-10-24.

### Not built, for now

- **Archives inside archives.** A zip or tar member that is itself a zip or
  tar is searched as bytes. The one level of nesting is for compression, and
  an archive in an archive is a container, not a compression.
- **Other stuffr formats** (cpio, ar, lha, arj, arc, zoo; brotli, snappy,
  lzip, lzma). Reachable the same way zip and `.Z` were, a cargo feature
  each, but none of them holds logs on any server trg runs on.

## Wishlist

### Features

Nothing at the moment. Adaptive `--load-limit`, the last item here, shipped
in 0.8.0.
