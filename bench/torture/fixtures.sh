#!/bin/sh
# Builds one archive per format in $WORK/fx (default target/torture/fx), each
# holding logs/a.log, 40,000 access-log lines with NEEDLE on its last line, and
# logs/b.log. Needs gzip, zstd, xz, bzip2, lz4, compress (ncompress) and zip.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
work=${WORK:-$root/target/torture}
rm -rf "$work/src" "$work/src2" "$work/fx"
mkdir -p "$work/src/logs" "$work/src2/logs" "$work/fx"
cd "$work"
python3 - <<'P'
import random
random.seed(1)
lines = [f'10.0.{random.randrange(256)}.{random.randrange(256)} - - [24/Aug/2026:10:{i%60:02d}:00 +0200] '
         f'"GET /p/{random.randrange(10**6)} HTTP/1.1" 200 {random.randrange(10**5)}\n' for i in range(40000)]
open('src/logs/a.log', 'w').write(''.join(lines) + 'LAST NEEDLE line\n')
open('src/logs/b.log', 'w').write(''.join(lines[:2000]))
P
(cd src && tar -cf ../fx/x.tar logs)
gzip -c fx/x.tar > fx/x.tgz
zstd -q -c fx/x.tar > fx/x.tar.zst
xz -c fx/x.tar > fx/x.tar.xz
bzip2 -c fx/x.tar > fx/x.tar.bz2
lz4 -q -c fx/x.tar > fx/x.tar.lz4
compress -c fx/x.tar > fx/x.tar.Z
(cd src && zip -q -r ../fx/x.zip logs)
(cd src && zip -q -0 -r ../fx/x0.zip logs)
gzip -c src/logs/a.log > fx/a.log.gz
lz4 -q -c src/logs/a.log > fx/a.log.lz4
# A rotated, gzipped log as a member of a tgz.
gzip -c src/logs/a.log > src2/logs/a.log.1.gz
cp src/logs/b.log src2/logs/
tar -C src2 -czf fx/rot.tgz logs
ls -l fx | awk 'NR > 1 {print $5, $9}'
