#!/bin/bash

SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

BUILD_ARGS=""
BUILD_DIR="debug"

if [[ "$1" == "--release" ]]; then
    BUILD_ARGS="--release"
    BUILD_DIR="release"
fi

cd "$SCRIPT_DIR"

(cd "anylinuxfs" && cargo build $BUILD_ARGS)
mkdir -p bin && cp "target/$BUILD_DIR/anylinuxfs" bin/

(cd "vmproxy" && cargo build $BUILD_ARGS)
mkdir -p bin/vmroot && cp "target/aarch64-unknown-linux-musl/$BUILD_DIR/vmproxy" bin/vmroot/

codesign --entitlements "anylinuxfs.entitlements" --force -s - bin/anylinuxfs
