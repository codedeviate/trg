#!/usr/bin/env python3
"""Size limits: 200,000 members, a 60 KB member name, a 2,000-deep path,
70,000 zip entries, and three gzip bombs that inflate to 4 GiB of NULs,
4 GiB of one line with no newline, and 2 GiB of empty lines.

And output limits, which `-c` never meets: an archive of 1.2 GB of lines
that all match, searched without `-c` so every one is printed, alone and as
four copies at -j4; a listing (-T) of 400 MB of member names; and an archive
of 300,000 members that each fail, for the errors.

    python3 bench/torture/bombs.py

Building the bombs writes about 40 MB and takes a few minutes; they are kept
in $WORK/bombs for the next run. Each case has an expected exit code, a time
limit, and a peak-memory limit (64 MB unless it says otherwise), measured
with /usr/bin/time where it exists (`time` package on Debian/Ubuntu). Exits
1 if a case misses any.
"""
import io
import os
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import time
import zipfile
import zlib

from common import TRG, WORK

d = os.path.join(WORK, "bombs")
os.makedirs(d, exist_ok=True)
os.chdir(d)


def bomb(path, chunk, n):
    c = zlib.compressobj(9, zlib.DEFLATED, 31)
    with open(path, "wb") as f:
        for _ in range(n):
            f.write(c.compress(chunk))
        f.write(c.flush())


if not os.path.exists("many.zip"):
    with tarfile.open("many.tgz", "w:gz", compresslevel=1) as t:
        for i in range(200000):
            b = f"line {i}\n".encode() + (b"NEEDLE\n" if i == 199999 else b"")
            ti = tarfile.TarInfo(f"d{i % 100}/m{i}.log")
            ti.size = len(b)
            t.addfile(ti, io.BytesIO(b))
    with tarfile.open("longname.tgz", "w:gz", format=tarfile.PAX_FORMAT) as t:
        for name in ["x" * 60000, "/".join(["d"] * 2000) + "/f.log"]:
            ti = tarfile.TarInfo(name)
            ti.size = 12
            t.addfile(ti, io.BytesIO(b"NEEDLE long\n"))
    bomb("zeros.gz", b"\0" * (1 << 24), 256)
    bomb("aaaa.gz", b"A" * (1 << 24), 256)
    bomb("newlines.gz", b"\n" * (1 << 24), 128)
    with zipfile.ZipFile("many.zip", "w", zipfile.ZIP_STORED) as z:
        for i in range(70000):
            z.writestr(f"m{i}.log", f"l{i}\n" + ("NEEDLE\n" if i == 69999 else ""))


class Repeat(io.RawIOBase):
    """`size` bytes of `line`, over and over, without holding them."""

    def __init__(self, line, size):
        self.block, self.left = line * (65536 // len(line)), size

    def readable(self):
        return True

    def readinto(self, b):
        n = min(len(b), len(self.block), self.left)
        b[:n] = self.block[:n]
        self.left -= n
        return n


if not os.path.exists("failing.tgz"):
    line = b"the quick brown fox jumps over the lazy dog GET /index.html 200 NEEDLE\n"
    size = len(line) * (1200000000 // len(line))
    with tarfile.open("matches.tgz", "w:gz", compresslevel=1) as t:
        ti = tarfile.TarInfo("big/huge.log")
        ti.size = size
        t.addfile(ti, io.BufferedReader(Repeat(line, size)))
    for i in range(2, 5):
        if not os.path.exists(f"matches{i}.tgz"):
            shutil.copy("matches.tgz", f"matches{i}.tgz")
    # 100,000 names of 4,000 bytes: a 400 MB listing.
    with tarfile.open("names.tgz", "w:gz", format=tarfile.PAX_FORMAT, compresslevel=1) as t:
        deep = "/".join(["d" * 99] * 40)
        for i in range(100000):
            ti = tarfile.TarInfo(f"{deep}/{i}.log")
            ti.size = 2
            t.addfile(ti, io.BytesIO(b"x\n"))
    # Every member the start of a gzip stream and nothing more: an error each.
    cut = zlib.compress(b"x" * 4096, 9, 31)[:12]
    with tarfile.open("failing.tgz", "w:gz", compresslevel=1) as t:
        for i in range(300000):
            ti = tarfile.TarInfo(f"logs/m{i}.log.gz")
            ti.size = len(cut)
            t.addfile(ti, io.BytesIO(cut))

four = ["matches.tgz", "matches2.tgz", "matches3.tgz", "matches4.tgz"]
# (arguments, expected exit, seconds allowed, MB allowed, why)
cases = [
    (["-c", "NEEDLE", "many.tgz"], 0, 10, 64, "200k members"),
    (["-c", "NEEDLE", "longname.tgz"], 0, 10, 64, "long name, deep path"),
    (["-c", "NEEDLE", "many.zip"], 0, 10, 64, "70k entries (zip64 count)"),
    (["-c", "NEEDLE", "zeros.gz"], 1, 30, 64, "4 GiB of NULs: binary, read on to the CRC"),
    (["-c", "NEEDLE", "aaaa.gz"], 2, 30, 64, "4 GiB line: stops at the 16 MiB line limit"),
    (["-c", "NEEDLE", "newlines.gz"], 1, 30, 64, "2 GiB of empty lines"),
    (["-N", "NEEDLE", "matches.tgz"], 0, 30, 64, "1.2 GB of matching lines, printed"),
    (["-j4", "NEEDLE"] + four, 0, 60, 160, "the same four times at -j4: a 64 MB budget"),
    (["-j4", "--no-sort", "NEEDLE"] + four, 0, 60, 160, "and unsorted"),
    (["-T", "names.tgz"], 0, 30, 64, "a 400 MB listing"),
    (["NEEDLE", "failing.tgz"], 2, 30, 64, "300k members that each fail: 100 errors and a count"),
]
timer = None
if os.path.exists("/usr/bin/time"):
    timer = ["/usr/bin/time", "-l" if platform.system() == "Darwin" else "-v"]
failed = False
for args, want, limit, mb_limit, why in cases:
    t = time.time()
    try:
        # Output is thrown away, not captured: it is the size that is tested.
        r = subprocess.run((timer or []) + [TRG] + args, stdout=subprocess.DEVNULL,
                           stderr=subprocess.PIPE, timeout=limit * 2)
        code, err = r.returncode, r.stderr.decode(errors="replace")
    except subprocess.TimeoutExpired:
        code, err = "TIMEOUT", ""
    secs = time.time() - t
    mb = None
    if m := re.search(r"(\d+)\s+maximum resident set size", err):
        mb = int(m.group(1)) / 2**20
    elif m := re.search(r"Maximum resident set size \(kbytes\): (\d+)", err):
        mb = int(m.group(1)) / 2**10
    bad = code != want or secs > limit or (mb is not None and mb > mb_limit)
    failed |= bad
    rss = f"{mb:6.1f} MB" if mb is not None else "     ? MB"
    name = " ".join(args)[:34]
    print(f"{name:34} exit {code!s:7} (want {want})  {secs:6.2f}s  {rss}  {why}{'  <<<' if bad else ''}")
sys.exit(1 if failed else 0)
