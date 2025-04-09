#!/bin/bash

SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

BUILD_ARGS=""
BUILD_DIR="debug"

if [[ "$1" == "--release" ]]; then
    BUILD_ARGS="--release"
    BUILD_DIR="release"
fi

cd "$SCRIPT_DIR"

export PKG_CONFIG_PATH="/opt/homebrew/opt/util-linux/lib/pkgconfig"
(cd "anylinuxfs" && cargo build $BUILD_ARGS)
mkdir -p bin && cp "anylinuxfs/target/$BUILD_DIR/anylinuxfs" bin/
codesign --entitlements "anylinuxfs.entitlements" --force -s - bin/anylinuxfs

ROOTFS_PATH=~/.anylinuxfs/alpine/rootfs

(cd "vmproxy" && cargo build $BUILD_ARGS)
mkdir -p libexec && cp "vmproxy/target/aarch64-unknown-linux-musl/$BUILD_DIR/vmproxy" libexec/
mkdir -p $ROOTFS_PATH && cp libexec/vmproxy $ROOTFS_PATH/

(cd "init-rootfs" && go build -o ../libexec/)
codesign --entitlements "anylinuxfs.entitlements" --force -s - libexec/init-rootfs
