#!/bin/sh
# This is expected to be run on a FreeBSD host
# (not really needed anymore as ../build-app.sh can do cross-compilation with rust nightly)

set -e

cd $(dirname "$0")

# hack to temporarily ignore the default toolchain specification
mv .cargo .cargo_
trap "mv .cargo_ .cargo" EXIT

cargo build --target-dir target-bsd --release
cp target-bsd/release/vmproxy vmproxy-bsd
