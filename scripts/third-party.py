#!/usr/bin/env python3
"""Write THIRD-PARTY.md: the licence notices for everything compiled into trg.

    scripts/third-party.py [TARGET] > THIRD-PARTY.md

TARGET is a target triple (default x86_64-unknown-linux-gnu). The release
workflow runs this for each binary it ships and puts the result in the
tarball, next to LICENSE.

What goes in: every crate the binary links, found from `cargo metadata` as the
normal (not dev, not build) dependencies of trg, minus proc-macro crates and
anything reachable only through them, which run at compile time and are not in
the binary. Then the C libraries bundled inside -sys crates, and the Rust
standard library.

Each crate's own licence files are copied, never a stock text, so copyright
lines are the authors' own. Where a crate offers a choice ("MIT OR
Apache-2.0"), the first of PREFERRED it offers is used and said so. Crates
with identical texts are listed together under one copy.

A crate this script cannot find a licence file for is an error, not a gap:
a new dependency fails the release until it is accounted for.
"""

import html
import json
import os
import re
import subprocess
import sys
from pathlib import Path

PREFERRED = ["MIT", "MIT-0", "0BSD", "Unlicense", "Zlib", "Apache-2.0", "CC0-1.0"]

# Which file names hold which licence, matched case-insensitively.
FILE_FOR = {
    "MIT": lambda n: "MIT" in n and "MIT-0" not in n,
    "MIT-0": lambda n: "MIT-0" in n,
    "0BSD": lambda n: "0BSD" in n,
    "Unlicense": lambda n: "UNLICENSE" in n,
    "Zlib": lambda n: "ZLIB" in n,
    "Apache-2.0": lambda n: "APACHE" in n and "LLVM" not in n,
    "CC0-1.0": lambda n: "CC0" in n,
}

LICENCE_FILE = re.compile(r"^(LICEN[CS]E|COPYING|UNLICENSE|COPYRIGHT|NOTICE)", re.I)
ALWAYS = re.compile(r"^(COPYRIGHT|NOTICE)", re.I)

# C code compiled into the binary from inside a -sys crate, with the files
# that carry its licence. Only the licence that applies to what is built:
# zstd is BSD or GPLv2 (BSD taken), and xz's GPL files cover its command-line
# tools, which liblzma-sys does not build.
BUNDLED_C = {
    "zstd-sys": ("zstd (C library)", ["zstd/LICENSE"]),
    "libz-ng-sys": ("zlib-ng (C library)", ["src/zlib-ng/LICENSE.md"]),
    "liblzma-sys": ("liblzma from XZ Utils (C library)", ["xz/COPYING", "xz/COPYING.0BSD"]),
}


def shipped_packages(target):
    meta = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--locked",
         "--filter-platform", target],
    ))
    pkgs = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    root = meta["resolve"]["root"]

    def proc_macro(pid):
        return any("proc-macro" in t["kind"] for t in pkgs[pid]["targets"])

    seen, stack = set(), [root]
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        for dep in nodes[pid]["deps"]:
            normal = any(k["kind"] is None for k in dep["dep_kinds"])
            if normal and not proc_macro(dep["pkg"]):
                stack.append(dep["pkg"])
    seen.discard(root)
    return sorted((pkgs[p] for p in seen), key=lambda p: (p["name"], p["version"]))


def choose(expr):
    """The licence used, or None when every file applies (an AND, or one
    licence)."""
    if expr is None:
        return None
    alts = [a.strip() for a in re.split(r"\s+OR\s+|/", expr)]
    if len(alts) < 2 or any(" AND " in a or "(" in a or ")" in a for a in alts):
        return None
    for want in PREFERRED:
        if want in alts:
            return want
    return None


def licence_texts(pkg):
    root = Path(pkg["manifest_path"]).parent
    files = sorted(f for f in root.iterdir() if f.is_file() and LICENCE_FILE.match(f.name))
    if pkg.get("license_file"):
        files.append(root / pkg["license_file"])
    used = choose(pkg["license"])
    if used:
        picked = [f for f in files if FILE_FOR[used](f.name.upper())]
        picked += [f for f in files if ALWAYS.match(f.name) and f not in picked]
    else:
        picked = files
    if not picked:
        sys.exit(f"third-party: no licence file for {pkg['name']} {pkg['version']} "
                 f"({pkg['license']}) in {root}")
    return used, [(f.name, f.read_text(errors="replace").strip()) for f in picked]


def std_notice():
    """The standard library's own notice, from the toolchain that built trg.

    COPYRIGHT-library.html goes on to list "out-of-tree dependencies", but
    that list covers the whole toolchain build (clap, darling, env_logger …),
    not what a Rust binary links, and is 1.4 MB. So this takes the in-tree
    part, which is std itself, and the licence texts it names.
    """
    sysroot = subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip()
    docs = Path(sysroot, "share/doc/rust")
    page = docs / "COPYRIGHT-library.html"
    if not page.exists():
        sys.exit(f"third-party: {page} not found; the Rust standard library's "
                 "notices come from it")
    text = re.sub(r"<(script|style)\b.*?</\1>", "", page.read_text(), flags=re.S | re.I)
    text = re.sub(r"<br\s*/?>|</(p|li|h\d|div|tr|pre)>", "\n", text, flags=re.I)
    text = html.unescape(re.sub(r"<[^>]+>", "", text))
    lines = [l.rstrip() for l in text.splitlines()]
    out, blank = [], False
    for l in lines:
        if l.strip():
            out.append(l)
            blank = False
        elif not blank:
            out.append("")
            blank = True
    text = "\n".join(out)
    # Each heading appears twice, in the table of contents and over its
    # section; the body runs from the second "Short version" to the second
    # "Out-of-tree dependencies".
    def second(word):
        return text.find(word, text.find(word) + 1)
    start, cut = second("Short version for non-lawyers"), second("Out-of-tree dependencies")
    if not 0 < start < cut:
        sys.exit(f"third-party: {page} no longer has the layout this script reads")
    parts = [text[start:cut].strip()]
    for name in ["MIT.txt", "Unicode-3.0.txt"]:
        parts.append(f"--- {name} ---\n\n{(docs / 'licenses' / name).read_text().strip()}")
    return "\n\n".join(parts)


def fence(text):
    return f"~~~~text\n{text}\n~~~~"


def main():
    target = sys.argv[1] if len(sys.argv) > 1 else "x86_64-unknown-linux-gnu"
    pkgs = shipped_packages(target)

    groups = {}  # licence text -> [(crate, expression, used)]
    for p in pkgs:
        used, texts = licence_texts(p)
        body = "\n\n".join(f"--- {name} ---\n\n{text}" for name, text in texts)
        groups.setdefault(body, []).append((f"{p['name']} {p['version']}", p["license"], used))

    print("# Third-party software in trg\n")
    print(f"The `trg` binary for `{target}` contains the software listed here. Each")
    print("entry gives the licence it is distributed under and the licence text the")
    print("authors ship with it. Where a crate offers a choice of licences, the one")
    print("this distribution uses is named. trg itself is MIT; see LICENSE.\n")
    print("Generated by `scripts/third-party.py` from `Cargo.lock`.\n")

    print("## Rust crates\n")
    for body, crates in sorted(groups.items(), key=lambda g: g[1][0][0]):
        for name, expr, used in crates:
            note = f", used under {used}" if used else ""
            print(f"- {name}: {expr}{note}")
        print()
        print(fence(body))
        print()

    # A crate with `links` builds or links native code, whose licence is not in
    # the crate's own files. Each one must be in BUNDLED_C.
    unknown = [p["name"] for p in pkgs if p.get("links") and p["name"] not in BUNDLED_C]
    if unknown:
        sys.exit(f"third-party: {', '.join(unknown)} link native code; add the bundled "
                 "library's licence files to BUNDLED_C")

    print("## C libraries compiled in\n")
    by_name = {p["name"]: p for p in pkgs}
    for crate, (title, rels) in BUNDLED_C.items():
        p = by_name.get(crate)
        if p is None:
            continue
        root = Path(p["manifest_path"]).parent
        texts = []
        for rel in rels:
            f = root / rel
            if not f.exists():
                sys.exit(f"third-party: {crate}: {rel} not found; has the bundled C moved?")
            texts.append(f"--- {rel} ---\n\n{f.read_text(errors='replace').strip()}")
        print(f"### {title}, bundled in {p['name']} {p['version']}\n")
        print(fence("\n\n".join(texts)))
        print()

    print("## The Rust standard library\n")
    print(fence(std_notice()))


if __name__ == "__main__":
    main()
