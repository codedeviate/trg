#!/usr/bin/env bash
# Compares trg against the inflate floor, zgrep and rg on wall clock, CPU,
# peak RSS and page-cache residency.
#
#   ./bench/compare.sh [CORPUS_DIR] [PATTERN]
#
# Env:
#   TRG         trg binary (default ./target/release/trg)
#   TRG_ZLIBNG  optional second trg binary built with --features zlib-ng
#   REPS        repetitions per row (default 3); the minimum is reported
#
# Every row starts from a cold page cache: posix_fadvise(POSIX_FADV_DONTNEED)
# over every archive, verified with mincore before the run starts. Without that
# the second tool measured would read 610 MB out of RAM and win on wall clock
# for reasons that have nothing to do with the tool.
#
# CPU and peak RSS come from wait4(2) on the child, so they cover the process
# and the descendants it waited for — that is how a `gzip | grep` pipeline
# such as zgrep gets accounted. ru_maxrss is a maximum over the pipeline's
# members, not a sum.
set -euo pipefail

corpus=${1:-/tmp/trgbench/corpus}
pat=${2:-reqid=ZZB0RvmI4TWPrNPs5Rb}
trg=${TRG:-./target/release/trg}
trg_ng=${TRG_ZLIBNG:-}
reps=${REPS:-3}

exec python3 - "$corpus" "$pat" "$trg" "$trg_ng" "$reps" <<'PY'
import ctypes, glob, os, resource, subprocess, sys, time

corpus, pat, trg, trg_ng, reps = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4], int(sys.argv[5])
files = sorted(glob.glob(os.path.join(corpus, "*.tgz")))
if not files:
    sys.exit(f"no archives under {corpus}")

libc = ctypes.CDLL("libc.so.6", use_errno=True)
# Without explicit restypes ctypes assumes c_int and truncates the 64-bit
# address mmap returns, which turns every mincore() call into EINVAL and every
# residency figure into a silent zero.
libc.mmap.restype = ctypes.c_void_p
libc.mmap.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.c_int,
                      ctypes.c_int, ctypes.c_int, ctypes.c_long]
libc.munmap.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
libc.mincore.argtypes = [ctypes.c_void_p, ctypes.c_size_t,
                         ctypes.POINTER(ctypes.c_ubyte)]
libc.posix_fadvise.argtypes = [ctypes.c_int, ctypes.c_long, ctypes.c_long,
                               ctypes.c_int]
PAGE = os.sysconf("SC_PAGE_SIZE")
PROT_READ, MAP_PRIVATE = 1, 2
POSIX_FADV_DONTNEED = 4


def evict():
    for p in files:
        fd = os.open(p, os.O_RDONLY)
        try:
            libc.posix_fadvise(fd, 0, 0, POSIX_FADV_DONTNEED)
        finally:
            os.close(fd)


def resident_bytes():
    """Bytes of the corpus currently held in the page cache, via mincore(2)."""
    total = 0
    for p in files:
        fd = os.open(p, os.O_RDONLY)
        try:
            size = os.fstat(fd).st_size
            if size == 0:
                continue
            addr = libc.mmap(None, size, PROT_READ, MAP_PRIVATE, fd, 0)
            if addr is None or ctypes.c_long(addr).value == -1:
                raise OSError(ctypes.get_errno(), "mmap")
            npages = (size + PAGE - 1) // PAGE
            vec = (ctypes.c_ubyte * npages)()
            try:
                if libc.mincore(ctypes.c_void_p(addr), ctypes.c_size_t(size), vec) != 0:
                    raise OSError(ctypes.get_errno(), "mincore")
                total += sum(1 for b in vec if b & 1) * PAGE
            finally:
                libc.munmap(ctypes.c_void_p(addr), ctypes.c_size_t(size))
        finally:
            os.close(fd)
    return total


def run(label, cmd):
    best = None
    for _ in range(reps):
        evict()
        cold = resident_bytes()
        t0 = time.monotonic()
        p = subprocess.Popen(["sh", "-c", cmd],
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        _, status, ru = os.wait4(p.pid, 0)
        wall = time.monotonic() - t0
        code = os.waitstatus_to_exitcode(status) if os.WIFEXITED(status) else -os.WTERMSIG(status)
        p.returncode = code  # the child is already reaped; keep Popen quiet
        after = resident_bytes()
        row = (wall, ru.ru_utime, ru.ru_stime, ru.ru_maxrss / 1024.0,
               after / 1e6, cold / 1e6, code)
        if best is None or row[1] + row[2] < best[1] + best[2]:
            best = row
    wall, u, s, rss, res, cold, code = best
    warn = "" if cold < 8e6 else f"  !! {cold:.0f} MB still cached before the run"
    print(f"{label:<34} {wall:7.2f} {u:7.2f} {s:6.2f} {u + s:7.2f} "
          f"{rss:8.1f} {res:9.1f}  {code}{warn}", flush=True)


raw = sum(os.path.getsize(f) for f in files)
print(f"corpus: {len(files)} archives, {raw / 1e6:.0f} MB compressed, pattern {pat!r}")
print(f"reps: {reps} (best by total CPU); cores: {os.cpu_count()}")
print()
print(f"{'strategy':<34} {'wall':>7} {'user':>7} {'sys':>6} {'cpu':>7} "
      f"{'peakRSS':>8} {'cached':>9}  rc")
print("-" * 92)

g = os.path.join(corpus, "*.tgz")
run("cat > /dev/null (I/O floor)", f"cat {g}")
run("gzip -dc (inflate floor)", f"gzip -dc {g}")
run("pigz -dc (zlib, threaded)", f"pigz -dc {g}")
run("pigz -p1 -dc (zlib floor)", f"pigz -p1 -dc {g}")
run("zgrep -a", f"zgrep -a '{pat}' {g}")
run("rg -za -j1", f"rg -za -j1 '{pat}' {corpus}/")
run("rg -za (all cores)", f"rg -za '{pat}' {corpus}/")
run("trg -j1 (default backend)", f"{trg} -j1 '{pat}' {corpus}/")
run("trg -j1 --inflate=buffer 1G", f"{trg} -j1 --inflate=buffer --inflate-budget 1G '{pat}' {corpus}/")
run("trg -j1 --no-drop-cache", f"{trg} -j1 --no-drop-cache '{pat}' {corpus}/")
run("trg --turbo", f"{trg} --turbo '{pat}' {corpus}/")
if trg_ng:
    run("trg -j1 (zlib-ng)", f"{trg_ng} -j1 '{pat}' {corpus}/")
    run("trg -j1 --inflate=buffer (ng)",
        f"{trg_ng} -j1 --inflate=buffer --inflate-budget 1G '{pat}' {corpus}/")
    run("trg --turbo (zlib-ng)", f"{trg_ng} --turbo '{pat}' {corpus}/")
PY
