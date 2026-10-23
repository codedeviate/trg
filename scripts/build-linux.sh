#!/bin/sh
# Cross-build a Linux release binary of trg to copy onto a server by hand.
#
#     scripts/build-linux.sh            # x86_64 (e.g. Debian 13 cloud-amd64)
#     scripts/build-linux.sh aarch64
#
# Links against glibc 2.17 symbols, so the binary runs on any glibc distro from
# CentOS 7 onwards, Debian 13 included. Needs cargo-zigbuild, zig and cmake
# (cmake for the zlib-ng backend), plus the rustup target:
#     rustup target add x86_64-unknown-linux-gnu
set -eu

arch=${1:-x86_64}
target=$arch-unknown-linux-gnu
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)

cargo zigbuild --release --target "$target.2.17"

mkdir -p dist
out=dist/trg-$version-$arch-linux
cp "target/$target/release/trg" "$out"
echo "$out"
