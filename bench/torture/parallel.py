#!/usr/bin/env python3
"""Scheduling: 300 archives of mixed formats, some cut and some corrupt,
searched at -j 1, 4, 16 and 64, then stopped early by a closed pipe and by
SIGINT and SIGTERM.

    python3 bench/torture/parallel.py

Every -j must give the same exit code and byte-identical stdout (output is
ordered), and the same set of error lines. Error *text* may differ by -j, as
a decoder's buffer state decides how far into a damaged archive it reads, so
only the archives named in errors are compared. A signal must end trg within
5 s, and `| head -1` within 5 s. Exits 1 on any difference.
"""
import os
import random
import re
import shutil
import signal
import subprocess
import sys
import time

from common import FX, TRG, WORK, lines, run

arch = os.path.join(WORK, "parallel")
shutil.rmtree(arch, ignore_errors=True)
os.makedirs(arch)
random.seed(5)
fx = ["x.tgz", "x.tar.zst", "x.tar.xz", "x.tar.bz2", "x.tar.lz4", "x.zip", "x.tar.Z"]
for i in range(300):
    src = random.choice(fx)
    d = open(os.path.join(FX, src), "rb").read()
    if i % 37 == 5:
        d = d[: len(d) // 2]
    if i % 53 == 7:
        d = bytearray(d)
        d[len(d) // 3] ^= 0x55
        d = bytes(d)
    open(os.path.join(arch, f"a{i:03d}-{src}"), "wb").write(d)

failed = False
named = lambda err: sorted(set(re.findall(rb"a\d{3}-[\w.]+", err)))
base = None
for j in (1, 4, 16, 64):
    t = time.time()
    code, out, err = run(["-j", str(j), "-c", "NEEDLE", arch], timeout=300)
    print(f"-j {j:<3} exit {code}  {time.time() - t:5.2f}s  out {lines(out)} lines  "
          f"errors name {len(named(err))} archives")
    if base is None:
        base = (code, out, named(err))
    elif (code, out, named(err)) != base:
        failed = True
        print(f"   differs from -j 1")

for j in (1, 16, 64):
    t = time.time()
    p = subprocess.Popen(f"'{TRG}' -j {j} GET '{arch}' 2>/dev/null | head -1 >/dev/null", shell=True)
    try:
        p.wait(5)
        print(f"-j {j:<3} | head -1   done in {time.time() - t:.2f}s")
    except subprocess.TimeoutExpired:
        failed = True
        p.kill()
        print(f"-j {j:<3} | head -1   still running after 5 s  <<<")

for sig in (signal.SIGINT, signal.SIGTERM):
    p = subprocess.Popen([TRG, "-j", "16", "GET", arch], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(0.3)
    p.send_signal(sig)
    t = time.time()
    try:
        p.wait(5)
        print(f"{sig.name:7} exit {p.returncode} in {time.time() - t:.2f}s")
    except subprocess.TimeoutExpired:
        failed = True
        p.kill()
        print(f"{sig.name:7} still running after 5 s  <<<")
sys.exit(1 if failed else 0)
