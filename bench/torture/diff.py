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
import os
import random
import subprocess
import sys
import tarfile

from common import TRG, WORK

N = int(sys.argv[1]) if len(sys.argv) > 1 else 1000
random.seed(int(sys.argv[2]) if len(sys.argv) > 2 else 1)
d = os.path.join(WORK, "diff")
os.makedirs(d, exist_ok=True)
os.chdir(d)

WORDS = ["GET", "POST", "error", "Error", "ERROR", "foo", "foobar", "bar", "é", "ü", "日本", "ǅ", "a.b",
         "x+y", "[1]", "(", "\t", "  ", "NEEDLE", "needle", "ß", "İ"]
PATS = ["GET", "error", "(?i)error", "^GET", "foo$", "bar\\b", "\\bfoo", "é", "日本", "ǅ", "a.b", "x\\+y",
        "\\[1\\]", "^$", "^", "$", "o+", "\\w+", "\\s{2}", "NEEDLE|foo", "[[:upper:]]{3}", "\\p{Han}", "ß",
        "(?m)^foo", "r$", "\\r$"]
FLAGS = [[], ["-i"], ["-w"], ["-v"], ["-o"], ["-c"], ["-F"], ["-S"], ["-C", "1"], ["-A", "2"], ["-B", "1"],
         ["-m", "2"], ["-n"], ["-N"], ["--crlf"]]


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
    sep = random.choice(["\n", "\n", "\r\n"])
    return (sep.join(lines) + random.choice(["", sep])).encode()


old_rg = int(subprocess.run(["rg", "--version"], capture_output=True, text=True).stdout.split()[1].split(".")[0]) < 15
if old_rg:
    print("rg older than 15: skipping -c with -o")
fails = 0
for it in range(N):
    names = [f"a{i}.log" for i in range(random.choice([1, 1, 2, 3]))]
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
    # -j1: rg prints files in the order given only on one thread.
    rg = subprocess.run(["rg", "-j1", "-H", "--no-heading", "--color=never", "-n"] + fl + ["-e", pat] + names,
                        capture_output=True)
    tg = subprocess.run([TRG, "--color=never"] + fl + ["-e", pat, "a.tgz"], capture_output=True)
    want = rg.stdout
    got = tg.stdout.replace(b"a.tgz:", b"")
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
        for name, body in zip(names, bodies):
            open(f"fail{fails}-{name}", "wb").write(body)
print(f"{N} cases: {fails} differ"
      + (f"; failing inputs in {d}" if fails else ""))
sys.exit(1 if fails else 0)
