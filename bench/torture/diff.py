#!/usr/bin/env python3
"""Differential test against ripgrep: random small logs (Unicode, CRLF, long
lines, empty lines, no final newline), random patterns and random flag sets.
Each case is one to three logs: members of one tgz for trg, plain files for
rg, and the outputs must match byte for byte, exit code included, `--`
between files included.

    python3 bench/torture/diff.py [N] [SEED]     # default 1000, 1

trg follows rg 15. With an older rg, cases that combine -c and -o are skipped:
rg 14 counted lines there, not matches. Exits 1 on any difference.
"""
import io
import json
import os
import random
import shutil
import subprocess
import sys
import tarfile

from common import TRG, WORK

N = int(sys.argv[1]) if len(sys.argv) > 1 else 1000
random.seed(int(sys.argv[2]) if len(sys.argv) > 2 else 1)
d = os.path.join(WORK, "diff")
shutil.rmtree(d, ignore_errors=True)
os.makedirs(d)
os.chdir(d)

WORDS = ["GET", "POST", "error", "Error", "ERROR", "foo", "foobar", "bar", "é", "ü", "日本", "ǅ", "a.b",
         "x+y", "[1]", "(", "\t", "  ", "NEEDLE", "needle", "ß", "İ"]
PATS = ["GET", "error", "(?i)error", "^GET", "foo$", "bar\\b", "\\bfoo", "é", "日本", "ǅ", "a.b", "x\\+y",
        "\\[1\\]", "^$", "^", "$", "o+", "\\w+", "\\s{2}", "NEEDLE|foo", "[[:upper:]]{3}", "\\p{Han}", "ß",
        "(?m)^foo", "r$", "\\r$"]
FLAGS = [[], ["-i"], ["-w"], ["-v"], ["-o"], ["-c"], ["-F"], ["-S"], ["-C", "1"], ["-A", "2"], ["-B", "1"],
         ["-m", "2"], ["-n"], ["-N"], ["--crlf"], ["-l"], ["-q"], ["-a"], ["--json"],
         ["--color", "always"], ["-e", "GET"]]


def unjson(b):
    """JSON Lines minus what cannot match: timings, and rg's closing summary
    of the whole run, which trg does not write."""
    def strip(v):
        if isinstance(v, dict):
            return {k: strip(x) for k, x in v.items() if k not in ("elapsed", "elapsed_total", "bytes_printed")}
        return [strip(x) for x in v] if isinstance(v, list) else v
    recs = [strip(json.loads(l)) for l in b.splitlines()]
    return "\n".join(json.dumps(r, sort_keys=True) for r in recs if r["type"] != "summary").encode()


def text():
    lines = []
    for _ in range(random.randint(0, 60)):
        k = random.random()
        if k < 0.1:
            lines.append("")
        elif k < 0.15:
            lines.append(" ".join(random.choices(WORDS, k=200)))
        else:
            lines.append(" ".join(random.choices(WORDS, k=random.randint(1, 8))))
    if random.random() < 0.1:
        lines.insert(random.randint(0, len(lines)), "bin\0ary")
    sep = random.choice(["\n", "\n", "\r\n"])
    return (sep.join(lines) + random.choice(["", sep])).encode()


old_rg = int(subprocess.run(["rg", "--version"], capture_output=True, text=True).stdout.split()[1].split(".")[0]) < 15
if old_rg:
    print("rg older than 15: skipping -c with -o")
fails = 0
for it in range(N):
    # In a directory rg walks, as members are files trg finds rather than
    # files it was handed: rg skips a binary file it finds, where it would
    # search one named on the command line.
    shutil.rmtree("m", ignore_errors=True)
    os.mkdir("m")
    names = [f"m/a{i}.log" for i in range(random.choice([1, 1, 2, 3]))]
    bodies = [text() for _ in names]
    with tarfile.open("a.tgz", "w:gz") as t:
        for name, body in zip(names, bodies):
            open(name, "wb").write(body)
            ti = tarfile.TarInfo(name)
            ti.size = len(body)
            t.addfile(ti, io.BytesIO(body))
    pat = random.choice(PATS)
    fl = [f for f in random.sample(FLAGS, random.randint(0, 3)) for f in f]
    if "-F" in fl and pat.startswith("(?"):
        continue
    if old_rg and "-c" in fl and "-o" in fl:
        continue
    # Two output modes: trg lets the most suppressive win (-q, -l, -c,
    # --json), rg has its own order, and still writes its JSON summary.
    if len({"-q", "-l", "-c", "--json"} & set(fl)) > 1:
        continue
    # -j1: rg prints files in the order given only on one thread.
    rg = subprocess.run(["rg", "-j1", "--no-ignore", "--hidden", "--sort=path", "--no-heading", "--color=never", "-n"] + fl
                        + ["-e", pat, "m"], capture_output=True)
    tg = subprocess.run([TRG, "--color=never"] + fl + ["-e", pat, "a.tgz"], capture_output=True)
    want = rg.stdout
    got = tg.stdout.replace(b"a.tgz:", b"")
    if "--json" in fl:
        want, got = unjson(want), unjson(got)
    if (want, rg.returncode) == (got, tg.returncode):
        continue
    fails += 1
    if fails <= 20:
        print("--- DIFF", fl, repr(pat), "rg exit", rg.returncode, "trg exit", tg.returncode, tg.stderr[:200])
        w, g = want.splitlines(), got.splitlines()
        for i in range(max(len(w), len(g))):
            a = w[i] if i < len(w) else None
            b = g[i] if i < len(g) else None
            if a != b:
                print("   rg :", a and a[:150])
                print("   trg:", b and b[:150])
                break
        os.makedirs(f"fail{fails}/m")
        for name, body in zip(names, bodies):
            open(f"fail{fails}/{name}", "wb").write(body)
        shutil.copy("a.tgz", f"fail{fails}/a.tgz")
        open(f"fail{fails}/cmd", "w").write(repr((fl, pat)) + "\n")
print(f"{N} cases: {fails} differ"
      + (f"; failing inputs in {d}" if fails else ""))
sys.exit(1 if fails else 0)
