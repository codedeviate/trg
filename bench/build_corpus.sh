#!/usr/bin/env bash
# Rebuilds the benchmark corpus from the container's real Apache logs.
#
# Content is varied per copy on purpose. GNU tar dedups identical inodes into
# hardlink entries, and trg skips hardlink members by design, so a corpus built
# from 45 copies of one file would be inflated once and "searched" 44 times for
# free — a fake corpus that flatters every tool that walks it.
#
# Result: 20 archives, ~610 MB compressed, ~11.5 GB raw, 136 entries each.
set -euo pipefail

out=${1:-/tmp/trgbench}
src=${SRC:-/var/log/apache2}

for f in SE.requests SE.log SE.error; do
  [ -r "$src/$f" ] || { echo "missing $src/$f" >&2; exit 1; }
done

rm -rf "$out"
mkdir -p "$out/stage/logs" "$out/corpus"

for i in $(seq -w 1 45); do
  sed "s/shop=www/shop=vhost$i/" "$src/SE.requests" > "$out/stage/logs/vhost$i.requests"
  sed "s/shop=www/shop=vhost$i/" "$src/SE.log"      > "$out/stage/logs/vhost$i.access.log"
  cp "$src/SE.error" "$out/stage/logs/vhost$i.error.log"
done

tar -czf "$out/one.tgz" -C "$out/stage" logs
for i in $(seq -w 1 20); do cp "$out/one.tgz" "$out/corpus/access-2026-08-$i.tgz"; done
rm -rf "$out/stage" "$out/one.tgz"

echo "members per archive: $(tar -tzf "$out/corpus/access-2026-08-01.tgz" | wc -l)"
du -sh "$out/corpus"
