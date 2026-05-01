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

HOST_OS="$(uname -s)"

# On Linux hosts the macOS Homebrew LLVM linker path pinned in
# vmproxy/.cargo/config.toml doesn't exist, and the -Clinker-plugin-lto flag
# requires rustc's LLVM version to match the distro clang — typically not the
# case. Override both via --config (linker, replaces scalar) and RUSTFLAGS env
# var (rustflags, replaces per-target rustflags per cargo precedence rules).
VMPROXY_LINUX_LINKER_CFG=()
VMPROXY_BSD_LINKER_CFG=()
# Empty arrays expand to nothing, so the cargo invocations below run without
# any env-prefix on macOS (preserving the per-target rustflags from
# vmproxy/.cargo/config.toml). On Linux we populate them with `env KEY=VAL`
# tokens; an array preserves the value as a single argv element through
# expansion, which a `${VAR:+RUSTFLAGS="$VAR"}` substitution does NOT —
# that gets word-split before the assignment is recognised.
VMPROXY_LINUX_RUSTFLAGS_ENV=()
VMPROXY_BSD_RUSTFLAGS_ENV=()
if [[ "$HOST_OS" == "Linux" ]]; then
    VMPROXY_LINUX_LINKER_CFG=(
        --config 'target.aarch64-unknown-linux-musl.linker="clang"'
    )
    VMPROXY_BSD_LINKER_CFG=(
        --config 'target.aarch64-unknown-freebsd.linker="clang"'
    )
    VMPROXY_LINUX_RUSTFLAGS_ENV=(
        env "RUSTFLAGS=-Clink-arg=--target=aarch64-unknown-linux-musl -Clink-arg=-fuse-ld=lld"
    )
    VMPROXY_BSD_RUSTFLAGS_ENV=(
        env "RUSTFLAGS=-Clink-arg=--target=aarch64-unknown-freebsd -Clink-arg=--sysroot=freebsd-sysroot -Clink-arg=-fuse-ld=lld -Clink-arg=-stdlib=libc++"
    )
fi

FEATURES="freebsd"

FEATURE_ARG=""
if [ -n "$FEATURES" ]; then
    FEATURE_ARG="-F $FEATURES"
fi

(cd "anylinuxfs" && cargo build $BUILD_ARGS $FEATURE_ARG)
mkdir -p bin && cp "anylinuxfs/target/$BUILD_DIR/anylinuxfs" bin/

if [[ "$HOST_OS" == "Darwin" ]]; then
    codesign --entitlements "anylinuxfs.entitlements" --force -s - bin/anylinuxfs
fi

(cd "vmproxy" && "${VMPROXY_LINUX_RUSTFLAGS_ENV[@]}" cargo build "${VMPROXY_LINUX_LINKER_CFG[@]}" $BUILD_ARGS $FEATURE_ARG)
mkdir -p libexec && cp "vmproxy/target/aarch64-unknown-linux-musl/$BUILD_DIR/vmproxy" libexec/

(cd "vmrunner-sys" && cargo build $BUILD_ARGS)
cp "vmrunner-sys/target/$BUILD_DIR/libvmrunner_sys.a" "vmrunner-sys/target/"
(cd "init-rootfs" && go build -ldflags="-w -s" -tags containers_image_openpgp -o ../libexec/)

if [[ "$HOST_OS" == "Darwin" ]]; then
    codesign --entitlements "anylinuxfs.entitlements" --force -s - libexec/init-rootfs
fi

(cd "freebsd-bootstrap" && CGO_ENABLED=0 GOOS=freebsd GOARCH=arm64 go build -tags netgo -ldflags '-extldflags "-static" -w -s' -o ../libexec/)

SYSROOT=freebsd-sysroot
(cd "vmproxy" \
    && test -d $SYSROOT \
    || (mkdir $SYSROOT && cd $SYSROOT \
        && curl -LO http://ftp.cz.freebsd.org/pub/FreeBSD/releases/arm64/14.3-RELEASE/base.txz \
        && tar xJf base.txz 2>/dev/null || true && rm base.txz) \
    && "${VMPROXY_BSD_RUSTFLAGS_ENV[@]}" cargo +nightly-2026-01-25 build "${VMPROXY_BSD_LINKER_CFG[@]}" -Z build-std --target aarch64-unknown-freebsd $BUILD_ARGS)
cp "vmproxy/target/aarch64-unknown-freebsd/$BUILD_DIR/vmproxy" libexec/vmproxy-bsd
