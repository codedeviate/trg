#!/usr/bin/env python3
"""Truncation sweep: cuts every fixture at N random offsets plus the edges
(block boundaries, the trailer) and searches for NEEDLE, which is on the last
line, so a cut always loses it.

    python3 bench/torture/cut.py [N]        # default 200 per fixture

Every cut must be an exit 2. Three exceptions are expected and tallied apart:
a .Z has no checksum or length, so a cut one is a 1 with its stderr note; a
plain .tar or a .Z cut after its last data byte, in the padding, still holds
every line and finds NEEDLE ("0, whole"); and a cut too short to hold its
format's signature reads as plain bytes ("short"). Exits 1 if anything else
turns up.
"""
import os
import random
import sys

from common import FX, WORK, fixtures, run

N = int(sys.argv[1]) if len(sys.argv) > 1 else 200
random.seed(7)
tmp = os.path.join(WORK, "cut")
os.makedirs(tmp, exist_ok=True)
bad = {}
for name in fixtures():
    data = open(os.path.join(FX, name), "rb").read()
    n = len(data)
    # A plain tar is known by "ustar" at offset 257; the rest by a few bytes.
    short = 262 if name.endswith(".tar") else 10
    unchecked = name.endswith(".tar") or name.endswith(".Z")
    edges = [1, 2, 3, 10, 100, 511, 512, 513, 1023, 1024, n - 1024, n - 512, n - 8, n - 2, n - 1]
    offs = sorted({o for o in edges + random.sample(range(1, n), min(N, n - 1)) if 0 < o < n})
    tally = {}
    p = os.path.join(tmp, name)
    for o in offs:
        open(p, "wb").write(data[:o])
        code, out, err = run(["NEEDLE", p], timeout=30)
        key = code
        if o < short and code in (1, 2):
            key = "short"
        elif code == 1 and name.endswith(".Z") and err:
            key = "1+note"
        elif code == 0 and unchecked and b"LAST NEEDLE line" in out:
            key = "0, whole"
        tally[key] = tally.get(key, 0) + 1
        if key not in (2, "short", "1+note", "0, whole"):
            bad.setdefault(name, []).append((o, n, key, out[-200:], err[-300:]))
    print(f"{name:12} {tally}")
for name, cases in bad.items():
    print(f"\n== {name}: {len(cases)} unexpected")
    for c in cases[:4]:
        print("  ", c)
sys.exit(1 if bad else 0)
