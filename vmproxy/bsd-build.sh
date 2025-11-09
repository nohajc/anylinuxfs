#!/bin/sh
# This is expected to be run on a FreeBSD host
# (aarch64 is a Tier 3 platform and as such doesn't support cross-compilation)

set -e

cd $(dirname "$0")

# hack to temporarily ignore the default toolchain specification
mv .cargo .cargo_
trap "mv .cargo_ .cargo" EXIT

cargo build --target-dir target-bsd --release
cp target-bsd/release/vmproxy vmproxy-bsd
