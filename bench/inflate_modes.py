#!/usr/bin/env python3
"""A/B of trg's --inflate modes on one corpus.

    python3 bench/inflate_modes.py CORPUS_DIR TRG [REPS]

Linux only (posix_fadvise). Every run starts from a cold page cache, and the
best total CPU of REPS is reported with peak RSS and the number of archives
that fell back from buffered to streaming. A fallback means that row measured
an attempt plus a full stream, not the buffered path: every buffered row in
the 2026-10-23 compare.sh tables did, because the archives inflated to 1.4 MB
more than the 1 GiB budget and compare.sh discarded stderr.
"""
import ctypes
import glob
import os
import subprocess
import sys
import tempfile

corpus, trg = sys.argv[1], sys.argv[2]
reps = int(sys.argv[3]) if len(sys.argv) > 3 else 3
files = sorted(glob.glob(os.path.join(corpus, "*.tgz")))
if not files:
    sys.exit(f"no archives under {corpus}")

libc = ctypes.CDLL("libc.so.6", use_errno=True)
libc.posix_fadvise.argtypes = [ctypes.c_int, ctypes.c_long, ctypes.c_long, ctypes.c_int]
POSIX_FADV_DONTNEED = 4


def evict():
    for p in files:
        fd = os.open(p, os.O_RDONLY)
        try:
            libc.posix_fadvise(fd, 0, 0, POSIX_FADV_DONTNEED)
        finally:
            os.close(fd)


def run(args):
    best = None
    for _ in range(reps):
        evict()
        # A file, not a pipe: reading a pipe only after wait4 deadlocks once the
        # child has written more than the pipe holds.
        with tempfile.TemporaryFile() as err:
            p = subprocess.Popen([trg, "-j1", *args, "-c", "reqid=ZZB0RvmI4TWPrNPs5Rb", corpus + "/"],
                                 stdout=subprocess.DEVNULL, stderr=err)
            _, _, ru = os.wait4(p.pid, 0)
            p.returncode = 0  # already reaped; keep Popen quiet
            err.seek(0)
            fallbacks = err.read().decode(errors="replace").count("fell back")
        cpu = ru.ru_utime + ru.ru_stime
        row = (cpu, ru.ru_utime, ru.ru_stime, ru.ru_maxrss / 1024, fallbacks)
        if best is None or cpu < best[0]:
            best = row
    return best


raw = sum(os.path.getsize(f) for f in files)
print(f"{os.path.basename(corpus.rstrip('/'))}: {len(files)} archives, {raw / 1e6:.0f} MB compressed")
for label, args in [("stream", ["--inflate=stream"]),
                    ("auto (default)", []),
                    ("buffer, 2G budget", ["--inflate=buffer", "--inflate-budget", "2G"])]:
    cpu, u, s, rss, fb = run(args)
    print(f"  {label:<20} cpu {cpu:6.2f}  user {u:6.2f}  sys {s:5.2f}  "
          f"peak RSS {rss:6.0f} MB  fallbacks {fb}", flush=True)
