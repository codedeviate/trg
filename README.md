# trg

Search inside compressed log archives — `.tgz` and `.tar.zst` — with
ripgrep's flags, per-member line numbers, and without trashing the page cache
of the server you are searching on.

```console
$ trg -i 'timeout' /var/log/archive/
/var/log/archive/2026-08-24.tgz:logs/vhost3.error.log:1182:[...] upstream timeout
/var/log/archive/2026-08-24.tgz:logs/vhost7.access.log.1.gz:40211:[...] 504 timeout
```

## Why

`zgrep -a` and `rg -za` both treat an archive as one long stream of text, so a
match reports a line number into the whole tarball, not into the log file it
came from. Both also pipe through the `gzip` binary, which inflates several
times slower than zlib, and both leave every archive they read resident in the
page cache — memory the production workload on that box needed.

`trg` reads the tar structure, so it reports `archive:member:line` with line
numbers counted per member and binary detection per member. It inflates
in-process with zlib-ng and drops archive pages from the cache as it goes.

Measured on 610 MB compressed / 11.5 GB raw of real Apache logs (Linux
aarch64):

| | wall | CPU | page cache left |
|---|---:|---:|---:|
| `zgrep -a` | 33.96 s | 31.84 s | 639 MB |
| `trg -j1` | 3.95 s | 3.81 s | 0 MB |
| `trg --turbo` | 0.84 s | — | 0 MB |

`--turbo` still uses less total CPU than `rg -za` on 12 cores.

## Features

- `archive:member/path:line:` output, line numbers correct per member.
- Compression detected from content, not the filename: gzip and zstd, each
  unwrapped one level, so a logrotate'd `access.log.1.gz` or `.zst` *inside*
  an archive is searched as text.
- ripgrep's flag names wherever the job is the same (`-i`, `-w`, `-F`, `-v`,
  `-C`, `-o`, `-c`, `-l`, `-q`, `--json`, …). `-g` filters *members* inside
  archives.
- Polite by default: `nice 10`, idle I/O priority, one archive at a time, and
  no page-cache footprint. `--turbo` lifts all of that when you need the answer
  now.
- An exit code you can trust (below).

## Install

Requires Rust (edition 2024) and, for the default zlib-ng backend, `cmake` and
a C compiler.

```sh
cargo install --path .
```

Without cmake, build the pure-Rust backend instead — slower, but still far
cheaper than the `gzip` binary:

```sh
cargo install --path . --no-default-features
```

## Usage

```sh
trg [OPTIONS] PATTERN PATH...
trg [OPTIONS] -e PATTERN... PATH...
trg [OPTIONS] -T PATH...            # list members, no search
```

At least one `PATH` is required: `trg` never searches the current directory on
its own.

```sh
trg 'POST /checkout' /var/log/archive/*.tgz
trg -g '*.error.log' -C 2 'segfault' /var/log/archive/
trg -T 2026-08-24.tar.zst
trg --turbo -c ' 500 ' /var/log/archive/
```

Run `trg --help` for every option, including `-j`, `--nice`,
`--no-drop-cache`, `--inflate` and `--load-limit`.

### Exit codes

| code | meaning |
|---|---|
| `0` | matches found, everything was read |
| `1` | no matches, everything was read |
| `2` | something went unread — an error, a truncated archive, or no `PATH` |

A `1` is a claim that the logs are clean, so `trg` only gives one when it read
everything. Never trust a `1` you did not get.

## Development

```sh
cargo test
```

The page-cache residency test runs on Linux only. `bench/` holds the corpus
builder and the comparison against `zgrep` and `rg`.

- [CHANGELOG.md](CHANGELOG.md) — what changed in each version
- [CONTRIBUTING.md](CONTRIBUTING.md) — commit conventions and versioning
- [OUT-OF-SCOPE.md](OUT-OF-SCOPE.md) — what `trg` will not do, and the wishlist
