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
# gzip (.tgz) or zstd (.tar.zst). zstd is made the way the servers have made
# archives since 2026-10: `tar --zstd` defaults, level 3, one frame, 2 MiB
# window (from `zstd -lv` on a production archive).
compress=${COMPRESS:-gzip}
case $compress in
  gzip) ext=tgz ;;
  zstd) ext=tar.zst ;;
  *) echo "COMPRESS must be gzip or zstd" >&2; exit 1 ;;
esac

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

if [ "$compress" = zstd ]; then
  tar -cf - -C "$out/stage" logs | zstd -q -3 -o "$out/one.$ext"
else
  tar -czf "$out/one.$ext" -C "$out/stage" logs
fi
for i in $(seq -w 1 20); do cp "$out/one.$ext" "$out/corpus/access-2026-08-$i.$ext"; done
rm -rf "$out/stage" "$out/one.$ext"

echo "members per archive: $(tar -tf "$out/corpus/access-2026-08-01.$ext" | wc -l)"
du -sh "$out/corpus"
