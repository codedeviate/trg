# Torture suite

Checks that `trg` keeps its exit-code contract on damaged, hostile and odd
input. The worst bug is a silent 1, which reports "no matches" about data
that was never read. Unit and integration tests cover single cases; this
suite sweeps them.

```sh
bench/torture/run.sh          # about a minute
bench/torture/run.sh full     # adds the size limits and bombs
bench/torture/linux.sh full   # the same in an ubuntu:24.04 container
```

Each script exits non-zero on a result it does not expect. They read `TRG`
(default `target/release/trg`) and `WORK` (default `target/torture`), and
can be run one at a time after `fixtures.sh`.

| Script | What it does | Passes when |
|---|---|---|
| `fixtures.sh` | builds one archive per format, NEEDLE on the last line | — |
| `cut.py` | truncates every fixture at ~100 offsets | every cut is a 2 (with exceptions, below) |
| `flip.py` | changes one byte at random offsets | every run is a 0 or a 2 |
| `edges.py` | symlink loops, unreadable paths, devices, odd flags and patterns | nothing crashes or hangs |
| `parallel.py` | 300 mixed archives, some damaged, at `-j` 1 to 64; SIGINT, SIGTERM, `\| head` | stdout and exit identical at every `-j`; signals end trg within 5 s |
| `diff.py` | random logs, patterns, flags and archive formats against rg | byte-identical output and exit code |
| `bombs.py` | 200k members, 70k zip entries, 60 KB names, 4 GiB gzip bombs | expected exit, within time, under 64 MB |

Expected exceptions, tallied apart rather than failed:

- **No checksum.** A plain `.tar` has no checksum on its data, and `.Z`
  has none at all. A byte changed in either can be a silent 1. trg notes
  this for `.Z` on stderr.
- **Cut in the padding.** A plain `.tar` or a `.Z` cut after its last data
  byte still holds every line, so finding NEEDLE is a 0.
- **Too short.** A file shorter than its format's signature, a few bytes,
  or under 262 bytes for a plain tar, reads as plain bytes.

Requirements: python3 and gzip, zstd, xz, bzip2, lz4, compress (ncompress)
and zip for the fixtures. `diff.py` needs rg 15 or newer; with an older rg
it skips `-c -o`, which rg 14 counted differently. `bombs.py` measures memory
with `/usr/bin/time` (the `time` package on Debian and Ubuntu).
