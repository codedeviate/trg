# targrep

`targrep` — run as `trg` — searches inside compressed log archives (`.tgz`,
`.tar.zst`, `.tar.xz`, `.tar.bz2`, `.tar.lz4`, `.tar.Z` and `.zip`) with
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

On x86-64 (a 4-core AMD EPYC GitHub runner, 1.16 GB compressed of synthetic
logs built to match the real ones' compression, via
`.github/workflows/bench.yml`):

| | wall | CPU | page cache left |
|---|---:|---:|---:|
| `zgrep -a` | 52.74 s | 56.78 s | 1162 MB |
| `rg -za` (all cores) | 14.55 s | 57.66 s | 1162 MB |
| `trg -j1` | 6.08 s | 6.07 s | 0 MB |
| `trg --turbo` | 2.76 s | 7.84 s | 0 MB |

9.4× less CPU than `zgrep`. Ratios move with the CPU — the same corpus gives
6.1× on a GitHub arm64 runner (Neoverse-N2) — so treat each table as a
measurement of that machine.

**zstd archives** narrow the CPU gap, because zstd decompresses fast for every
tool. Same runner type, 928 MB of `.tar.zst` made like the production archives
(`tar --zstd` defaults: one frame, 2 MiB window):

| | x86-64 CPU | arm64 CPU | page cache left |
|---|---:|---:|---:|
| `zstd -dc` (decompression alone) | 10.07 s | 6.96 s | 928 MB |
| `zstdgrep -a` | 23.64 s | 13.15 s | 928 MB |
| `rg -za` (all cores) | 20.69 s | 15.69 s | 928 MB |
| `trg -j1` | 9.95 s | 8.52 s | 0 MB |

On zstd, `trg` costs about what decompression alone costs, 1.5–2.4× less CPU
than `zstdgrep`. What stays the same is the rest: per-member line numbers, no
page cache left behind, and idle I/O priority — which on the x86 runner's slow
disk made `trg -j1` take longer on the wall clock (22 s against `zstdgrep`'s
12 s) while using under half the CPU. That is the trade it is built to make.

## Features

- `archive:member/path:line:` output, line numbers correct per member.
- Compression detected from content, not the filename: gzip, zstd, xz,
  bzip2, lz4 and Unix `compress`, each unwrapped one level, so a logrotate'd
  `access.log.1.gz`, `.zst`, `.xz`, `.bz2`, `.lz4` or `.Z` *inside* an archive
  is searched as text.
- `.zip` archives, searched member by member like a tar, and `.Z`, both
  through the [stuffr](https://github.com/codedeviate/stuffr) crates. A zip
  whose local headers name different files from its central directory (a
  "cloaked" zip) is searched as the directory has it, and is an exit 2.
- ripgrep's flag names wherever the job is the same (`-i`, `-w`, `-F`, `-v`,
  `-C`, `-o`, `-c`, `-l`, `-q`, `--json`, …). `-g` filters *members* inside
  archives, `--archive-glob` filters the archives a directory walk finds, and
  both take ripgrep's `!` exclusions (`-g '!*.error.log'`).
- Polite by default: `nice 10`, idle I/O priority, one archive at a time, and
  no page-cache footprint. `--turbo` runs one archive per CPU at normal
  priority when you need the answer now; reads stay at idle I/O priority and
  archives still leave the page cache (`--no-drop-cache` keeps them).
- An exit code you can trust (below).
- Safe to read on a terminal: log lines and member names are written by
  whoever sent the request, so on a tty their control bytes are shown as text
  (`\x1b[2J`) instead of being acted on — no retitled windows, cleared
  screens, clipboard writes or hidden lines. trg's own colours still work.
  Piped output stays byte-exact; `--escape always|never` overrides.

## Install

With Homebrew (builds from source):

```sh
brew install codedeviate/cli/targrep
```

From [crates.io](https://crates.io/crates/targrep) — the crate is `targrep`,
the binary it installs is `trg`:

```sh
cargo install targrep
```

Either way this needs Rust 1.88 or newer and, for the default zlib-ng backend,
`cmake` and a C compiler. From a checkout:

```sh
cargo install --path .
```

Without cmake, build the pure-Rust backend instead — slower, but still far
cheaper than the `gzip` binary:

```sh
cargo install --path . --no-default-features
```

### A Linux binary to copy onto a server

Every [GitHub release](https://github.com/codedeviate/trg/releases) has
prebuilt binaries for x86_64 and aarch64, each with a `.sha256`:

```sh
v=0.12.0   # or whichever release you want
curl -LO https://github.com/codedeviate/trg/releases/download/v$v/trg-$v-x86_64-linux.tar.gz
tar -xzf trg-$v-x86_64-linux.tar.gz   # trg-$v-x86_64-linux/trg, with LICENSE and THIRD-PARTY.md
```

To build one yourself, from macOS or any other host, with `cargo-zigbuild`,
`zig` and `cmake` installed:

```sh
scripts/build-linux.sh            # dist/trg-<version>-x86_64-linux
scripts/build-linux.sh aarch64    # dist/trg-<version>-aarch64-linux
```

The binary depends on nothing but glibc 2.17 or newer, so it runs on any
current Debian, Ubuntu or RHEL. Copy it over with `scp` and put it on the
`PATH` as `trg`.

Each tarball has a `THIRD-PARTY.md` with the licence notices of everything
compiled into the binary: the Rust crates, the bundled zstd, zlib-ng and
liblzma C code, and the Rust standard library. Keep it with the binary if you
pass the binary on. `scripts/third-party.py` writes it.

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
trg --archive-glob '2026-08-*' ' 500 ' /var/log/archive/   # August only, any depth
trg -T 2026-08-24.tar.zst
trg --turbo -c ' 500 ' /var/log/archive/
```

Run `trg --help` for every option, including `-j`, `--nice`,
`--no-drop-cache` and `--load-limit`, and `trg --examples` for worked
examples of all of them: patterns, member and archive globs, output modes,
every format, exit codes in scripts, and recipes such as the top client IPs
across a month of archives.

### Exit codes

| code | meaning |
|---|---|
| `0` | matches found, everything was read |
| `1` | no matches, everything was read |
| `2` | something went unread — an error, a truncated archive, no `PATH`, or an `--archive-glob` that matched nothing |

A `1` is a claim that the logs are clean, so `trg` only gives one when it read
everything. Never trust a `1` you did not get.

One format cannot back that claim fully: `.Z` records neither its length
nor a checksum, so a truncated `.Z` decodes cleanly to a shorter file. Cut at
every seventh byte, all 7,786 truncations of a 54 KB `.Z` read without an
error. A `.Z` with no match is therefore still a `1`, but it comes with a
note on stderr saying so. Every other format detects truncation.

Every diagnostic from inside an archive starts with that archive's path.

## Development

```sh
cargo test
```

The page-cache residency test runs on Linux only. `bench/` holds the corpus
builder and the comparison against `zgrep` and `rg`.

- [CHANGELOG.md](CHANGELOG.md) — what changed in each version
- [CONTRIBUTING.md](CONTRIBUTING.md) — commit conventions and versioning
- [OUT-OF-SCOPE.md](OUT-OF-SCOPE.md) — what `trg` will not do, and the wishlist

## License

MIT — see [LICENSE](LICENSE).
