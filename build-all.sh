#!/bin/bash

SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

BUILD_ARGS=""
BUILD_DIR="debug"

if [[ "$1" == "--release" ]]; then
    BUILD_ARGS="--release"
    BUILD_DIR="release"
fi

(cd "$SCRIPT_DIR/anylinuxfs" && cargo build $BUILD_ARGS)
(cd "$SCRIPT_DIR" && cp "target/$BUILD_DIR/anylinuxfs" bin/)

(cd "$SCRIPT_DIR/vmproxy" && cargo build $BUILD_ARGS)
(cd "$SCRIPT_DIR" && cp "target/aarch64-unknown-linux-musl/$BUILD_DIR/vmproxy" bin/)
