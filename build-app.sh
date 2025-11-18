#!/bin/bash

set -e

SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

BUILD_ARGS=""
BUILD_DIR="debug"

if [[ "$1" == "--release" ]]; then
    BUILD_ARGS="--release"
    BUILD_DIR="release"
fi

cd "$SCRIPT_DIR"

FEATURES=""

if [ -n "$FREEBSD" ]; then
    FEATURES="$FEATURES,freebsd"
fi

FEATURE_ARG=""
if [ -n "$FEATURES" ]; then
    FEATURE_ARG="-F $FEATURES"
fi

export PKG_CONFIG_PATH="/opt/homebrew/opt/util-linux/lib/pkgconfig"
(cd "anylinuxfs" && cargo build $BUILD_ARGS $FEATURE_ARG)
mkdir -p bin && cp "anylinuxfs/target/$BUILD_DIR/anylinuxfs" bin/
codesign --entitlements "anylinuxfs.entitlements" --force -s - bin/anylinuxfs

ROOTFS_PATH=~/.anylinuxfs/alpine/rootfs

(cd "vmproxy" && cargo build $BUILD_ARGS)
mkdir -p libexec && cp "vmproxy/target/aarch64-unknown-linux-musl/$BUILD_DIR/vmproxy" libexec/

(cd "init-rootfs" && go build -ldflags="-w -s" -tags containers_image_openpgp -o ../libexec/)
codesign --entitlements "anylinuxfs.entitlements" --force -s - libexec/init-rootfs

if [ -n "$FREEBSD" ]; then
    (cd "freebsd-bootstrap" && CGO_ENABLED=0 GOOS=freebsd GOARCH=arm64 go build -tags netgo -ldflags '-extldflags "-static" -w -s' -o ../libexec/)

    SYSROOT=freebsd-sysroot
    (cd "vmproxy" \
        && test -d $SYSROOT \
        || (mkdir $SYSROOT && cd $SYSROOT \
            && curl -LO http://ftp.cz.freebsd.org/pub/FreeBSD/releases/arm64/14.3-RELEASE/base.txz \
            && tar xJf base.txz 2>/dev/null || true && rm base.txz) \
        && cargo +nightly build -Z build-std --target aarch64-unknown-freebsd $BUILD_ARGS)
    cp "vmproxy/target/aarch64-unknown-freebsd/$BUILD_DIR/vmproxy" libexec/vmproxy-bsd
fi
