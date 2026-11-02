#!/usr/bin/env python3
"""Size limits: 200,000 members, a 60 KB member name, a 2,000-deep path,
70,000 zip entries, and three gzip bombs that inflate to 4 GiB of NULs,
4 GiB of one line with no newline, and 2 GiB of empty lines.

    python3 bench/torture/bombs.py

Building the bombs writes about 25 MB and takes a minute; they are kept in
$WORK/bombs for the next run. Each case has an expected exit code, a time
limit, and a peak-memory limit of 64 MB, measured with /usr/bin/time where
it exists (`time` package on Debian/Ubuntu). Exits 1 if a case misses any.
"""
import io
import os
import platform
import re
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

# (file, expected exit, seconds allowed, why)
cases = [
    ("many.tgz", 0, 10, "200k members"),
    ("longname.tgz", 0, 10, "long name, deep path"),
    ("many.zip", 0, 10, "70k entries (zip64 count)"),
    ("zeros.gz", 1, 30, "4 GiB of NULs: binary, read on to the CRC"),
    ("aaaa.gz", 2, 30, "4 GiB line: stops at the 16 MiB line limit"),
    ("newlines.gz", 1, 30, "2 GiB of empty lines"),
]
timer = None
if os.path.exists("/usr/bin/time"):
    timer = ["/usr/bin/time", "-l" if platform.system() == "Darwin" else "-v"]
failed = False
for f, want, limit, why in cases:
    t = time.time()
    try:
        r = subprocess.run((timer or []) + [TRG, "-c", "NEEDLE", f], capture_output=True, timeout=limit * 2)
        code, err = r.returncode, r.stderr.decode(errors="replace")
    except subprocess.TimeoutExpired:
        code, err = "TIMEOUT", ""
    secs = time.time() - t
    mb = None
    if m := re.search(r"(\d+)\s+maximum resident set size", err):
        mb = int(m.group(1)) / 2**20
    elif m := re.search(r"Maximum resident set size \(kbytes\): (\d+)", err):
        mb = int(m.group(1)) / 2**10
    bad = code != want or secs > limit or (mb is not None and mb > 64)
    failed |= bad
    rss = f"{mb:5.1f} MB" if mb is not None else "   ? MB"
    print(f"{f:14} exit {code!s:7} (want {want})  {secs:6.2f}s  {rss}  {why}{'  <<<' if bad else ''}")
sys.exit(1 if failed else 0)
