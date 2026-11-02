#!/usr/bin/env python3
"""Corruption sweep: changes one byte of a fixture at N random offsets and
searches for NEEDLE.

    python3 bench/torture/flip.py [N] [SEED] [FIXTURE...]   # default 200, 3, all

Each run must be a 0 (the damage missed the data, or a checksum-free byte) or
a 2 (the damage was found). A 1 means trg called a corrupt archive clean and
missed NEEDLE. A plain .tar has no checksum on data and a .Z none at all, so
for those a 1 is reported but not counted as a failure. Exits 1 on any
failure, crash or timeout.
"""
import os
import random
import sys

from common import FX, WORK, fixtures, run

N = int(sys.argv[1]) if len(sys.argv) > 1 else 200
random.seed(int(sys.argv[2]) if len(sys.argv) > 2 else 3)
names = sys.argv[3:] or fixtures()
os.makedirs(WORK, exist_ok=True)
p = os.path.join(WORK, "flipped")
failed = False
for name in names:
    d = open(os.path.join(FX, name), "rb").read()
    unchecked = name.endswith(".tar") or name.endswith(".Z")
    tally = {}
    odd = []
    for _ in range(N):
        o = random.randrange(len(d))
        x = bytearray(d)
        x[o] ^= random.choice([1, 2, 4, 8, 16, 32, 64, 128, 0xFF])
        open(p, "wb").write(x)
        code, _, err = run(["NEEDLE", p])
        tally[code] = tally.get(code, 0) + 1
        if code not in (0, 2):
            odd.append((o, code, err[-200:]))
    bad = [c for c in odd if not (unchecked and c[1] == 1)]
    failed |= bool(bad)
    note = " (no checksum: 1s expected)" if unchecked else ""
    print(f"{name:12} {tally}{note}")
    for c in bad[:3]:
        print("   ", c)
sys.exit(1 if failed else 0)
