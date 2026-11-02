#!/usr/bin/env python3
"""Odd inputs and odd flags: symlink loops, unreadable paths, a FIFO with no
writer, devices, empty and huge patterns, out-of-range numbers.

    python3 bench/torture/edges.py

Prints one line per case. A case fails when trg crashes (an exit outside
0-2, or a signal) or runs past 30 s; every case is expected to be a 0, 1 or
2. The FIFO with no writer is left out: it blocks, as any reader would.
Exits 1 on a failure. Read the output too: an exit in range can still be the
wrong one, and the error text is shown for that.
"""
import os
import shutil
import sys

from common import FX, WORK, lines, run

d = os.path.join(WORK, "edges")
shutil.rmtree(d, ignore_errors=True)
os.makedirs(os.path.join(d, "loop", "a"))
os.makedirs(os.path.join(d, "noperm"))
os.makedirs(os.path.join(d, "d"))
os.chdir(d)
os.symlink("..", "loop/a/up")
os.symlink("loop", "loop/self")
os.symlink("/nonexistent", "dangling")
for t in ("d/x.tgz", "noperm/x.tgz", "unreadable.tgz"):
    shutil.copy(os.path.join(FX, "x.tgz"), t)
open("alts.txt", "w").write("\n".join(str(i) for i in range(20000)))
os.chmod("noperm", 0)
os.chmod("unreadable.tgz", 0)

cases = [
    ["NEEDLE", "loop"], ["NEEDLE", "noperm"], ["NEEDLE", "unreadable.tgz"], ["NEEDLE", "dangling"],
    ["NEEDLE", "/dev/zero"], ["NEEDLE", "/dev/null"], ["NEEDLE", "d", "d", "d/x.tgz"],
    ["-e", "", "d"], ["", "d/x.tgz"], ["-c", "x*", "d/x.tgz"], ["-o", "", "d/x.tgz"],
    ["-m", "0", "NEEDLE", "d"], ["-m", "-1", "x", "d"], ["-C", "100000000", "NEEDLE", "d"],
    ["-A", "5", "-B", "5", "-v", ".", "d"], ["-j", "0", "NEEDLE", "d"], ["-j", "100000", "NEEDLE", "d"],
    ["--nice", "99", "NEEDLE", "d"], ["--nice", "-20", "NEEDLE", "d"],
    ["--load-limit", "-1", "NEEDLE", "d"], ["--load-limit", "nan", "NEEDLE", "d"],
    ["--load-limit", "inf", "NEEDLE", "d"], ["--archive-sep", "", "NEEDLE", "d"],
    ["--archive-sep", "::", "NEEDLE", "d"], ["-g", "", "NEEDLE", "d"], ["-g", "[", "NEEDLE", "d"],
    ["(", "d"], ["a" * 100000, "d"], ["|".join(str(i) for i in range(20000)), "d"],
    ["\\p{Greek}+", "d"], ["(?i)ǅ", "d"], ["-F", "-e", "NEEDLE", "--", "-d"],
    ["--color", "always", "--json", "NEEDLE", "d"], ["-q", "-l", "-c", "--json", "NEEDLE", "d"],
    ["-w", "-x", "NEEDLE", "d"], ["--crlf", "NEEDLE$", "d"], ["NEEDLE", "d", "nonexistent"],
    ["--archive-glob", "*.nope", "NEEDLE", "d"], ["-T"], ["-T", "d", "-e", "x"], ["NEEDLE"], [],
    ["--bogus"], ["-j"],
]
failed = False
try:
    for c in cases:
        code, out, err = run(c, timeout=30)
        bad = code not in (0, 1, 2)
        failed |= bad
        shown = " ".join(x if len(x) < 30 else f"{x[:12]}…({len(x)})" for x in c)
        err = err[:150].decode(errors="replace").replace("\n", "|")
        print(f"{shown:44} exit {code!s:7} out {lines(out):5}  {err}{'  <<<' if bad else ''}")
finally:
    os.chmod("noperm", 0o755)
    os.chmod("unreadable.tgz", 0o644)
sys.exit(1 if failed else 0)
