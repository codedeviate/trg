#!/usr/bin/env python3
"""Differential test against ripgrep: random logs (Unicode, CRLF, long lines,
empty lines, binary lines, no final newline, now and then a few hundred KB),
random patterns and random flag sets. Each case is one to three logs: members
of one archive, in a format picked at random, for trg; files in a directory
for rg. The outputs must match byte for byte, exit code included, `--`
between files included.

    python3 bench/torture/diff.py [N] [SEED]     # default 1000, 1

trg follows rg 15. With an older rg, cases that combine -c and -o are skipped:
rg 14 counted lines there, not matches, and reported 0 bytes_searched in
JSON under -m, so that field is not compared. Also skipped or allowed:
two output modes at once (each tool has its own precedence), rg's closing
JSON summary, and -c on a member that matched and then hit binary data.
Exits 1 on any difference; the inputs are kept in fail*/.
"""
import io
import json
import os
import random
import shutil
import subprocess
import sys
import tarfile
import zipfile

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


FORMATS = ["tgz", "tar", "tar.xz", "tar.bz2", "tar.zst", "tar.lz4", "tar.Z", "zip"]


def archive(names, bodies, fmt):
    """The logs as members of one archive in `fmt`; returns its name."""
    for old in [f for f in os.listdir(".") if f.startswith("a.")]:
        os.remove(old)
    if fmt == "zip":
        with zipfile.ZipFile("a.zip", "w", zipfile.ZIP_DEFLATED) as z:
            for name, body in zip(names, bodies):
                z.writestr(name, body)
        return "a.zip"
    mode = {"tgz": "w:gz", "tar.xz": "w:xz", "tar.bz2": "w:bz2"}.get(fmt, "w")
    out = "a." + fmt if fmt in ("tgz", "tar.xz", "tar.bz2") else "a.tar"
    with tarfile.open(out, mode) as t:
        for name, body in zip(names, bodies):
            ti = tarfile.TarInfo(name)
            ti.size = len(body)
            t.addfile(ti, io.BytesIO(body))
    # Output named explicitly: lz4 writes to stdout when stdout is a pipe.
    tool = {"tar.zst": ["zstd", "-q", "a.tar", "-o"], "tar.lz4": ["lz4", "-q", "a.tar"],
            "tar.Z": ["sh", "-c", "compress -c a.tar > \"$0\""]}
    if fmt in tool:
        out = "a." + fmt
        subprocess.run(tool[fmt] + [out], check=True)
        os.remove("a.tar")
    return out


def unjson(b):
    """JSON Lines minus what cannot match: timings, and rg's closing summary
    of the whole run, which trg does not write."""
    def strip(v):
        if isinstance(v, dict):
            return {k: strip(x) for k, x in v.items() if k not in DROP}
        return [strip(x) for x in v] if isinstance(v, list) else v
    recs = [strip(json.loads(l)) for l in b.splitlines()]
    return "\n".join(json.dumps(r, sort_keys=True) for r in recs if r["type"] != "summary").encode()


def text():
    lines = []
    # Now and then a log of a few hundred KB, past the searcher's 64 KB
    # buffer, so a NUL can come after matches.
    for _ in range(random.randint(0, 60) if random.random() < 0.95 else random.randint(3000, 12000)):
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
# Timings, and bytes_printed, which counts trg's longer archive:member paths.
DROP = {"elapsed", "elapsed_total", "bytes_printed"}
if old_rg:
    print("rg older than 15: skipping -c with -o, and JSON bytes_searched under -m")
    DROP.add("bytes_searched")
fails = 0
for it in range(N):
    # In a directory rg walks, as members are files trg finds rather than
    # files it was handed: rg skips a binary file it finds, where it would
    # search one named on the command line.
    shutil.rmtree("m", ignore_errors=True)
    os.mkdir("m")
    names = [f"m/a{i}.log" for i in range(random.choice([1, 1, 2, 3]))]
    bodies = [text() for _ in names]
    for name, body in zip(names, bodies):
        open(name, "wb").write(body)
    arc = archive(names, bodies, random.choice(FORMATS))
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
    tg = subprocess.run([TRG, "--color=never"] + fl + ["-e", pat, arc], capture_output=True)
    want = rg.stdout
    got = tg.stdout.replace(arc.encode() + b":", b"")
    if "--json" in fl:
        want, got = unjson(want), unjson(got)
    if (want, rg.returncode) == (got, tg.returncode):
        continue
    # Decided in OUT-OF-SCOPE.md: -c on a member that matched and then hit
    # binary data. rg prints no count and exits 1; trg prints none either,
    # but exits 0, as it matched, and says so on stderr.
    if want == got and (rg.returncode, tg.returncode) == (1, 0) and b"count suppressed" in tg.stderr:
        continue
    fails += 1
    if fails <= 20:
        print("--- DIFF", arc, fl, repr(pat), "rg exit", rg.returncode, "trg exit", tg.returncode, tg.stderr[:200])
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
        shutil.copy(arc, f"fail{fails}/{arc}")
        open(f"fail{fails}/cmd", "w").write(repr((fl, pat)) + "\n")
print(f"{N} cases: {fails} differ"
      + (f"; failing inputs in {d}" if fails else ""))
sys.exit(1 if fails else 0)
