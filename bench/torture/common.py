"""Shared by the torture scripts: where the binary and the work dir are, and a
run() that never hangs the sweep."""
import os
import subprocess

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TRG = os.environ.get("TRG") or os.path.join(ROOT, "target", "release", "trg")
WORK = os.environ.get("WORK") or os.path.join(ROOT, "target", "torture")
FX = os.path.join(WORK, "fx")


def run(args, timeout=60, **kw):
    """(exit code or "TIMEOUT", stdout, stderr) of trg with args."""
    try:
        r = subprocess.run([TRG] + args, capture_output=True, timeout=timeout,
                           stdin=subprocess.DEVNULL, **kw)
        return r.returncode, r.stdout, r.stderr
    except subprocess.TimeoutExpired:
        return "TIMEOUT", b"", b""


def fixtures():
    if not os.path.isdir(FX):
        raise SystemExit(f"no fixtures in {FX}: run bench/torture/fixtures.sh first")
    return sorted(os.listdir(FX))


def lines(b):
    return b.count(b"\n")
