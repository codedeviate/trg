#!/bin/sh
# The local half of a release build: the host binary plus both Linux binaries,
# so there is always a Linux trg of this version to copy onto a server.
#
#     scripts/release-build.sh
#
# Leaves target/release/trg and dist/trg-<version>-{x86_64,aarch64}-linux.
# Needs what scripts/build-linux.sh needs: cargo-zigbuild, zig, cmake and the
# two rustup targets.
set -eu
cd "$(dirname "$0")/.."

cargo build --release
for arch in x86_64 aarch64; do
    scripts/build-linux.sh "$arch"
done
