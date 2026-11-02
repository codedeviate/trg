#!/bin/sh
# Runs the torture suite on Linux, in a throwaway ubuntu:24.04 container:
#
#     bench/torture/linux.sh [full]
#
# Cross-builds the binary with scripts/build-linux.sh for this machine's
# architecture, mounts the repository read-only and works in the container's
# /tmp. Needs docker plus what build-linux.sh needs.
set -eu
cd "$(dirname "$0")/../.."
arch=$(uname -m)
[ "$arch" = arm64 ] && arch=aarch64
bin=$(scripts/build-linux.sh "$arch" | tail -n1)
docker run --rm -v "$PWD:/src:ro" -e TRG="/src/$bin" -e WORK=/tmp/torture ubuntu:24.04 sh -c '
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq >/dev/null
    apt-get install -y -qq python3 zstd xz-utils bzip2 lz4 zip ncompress ripgrep gzip time >/dev/null 2>&1
    echo "== $(uname -sr) $(uname -m), $(rg --version | head -n1)"
    sh /src/bench/torture/run.sh "$@"' sh "$@"
