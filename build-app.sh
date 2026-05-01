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
HOST_ARCH="$(uname -m)"

# Map uname -m output to the conventions we need:
#   RUST_ARCH: Rust target triple arch component (aarch64 or x86_64)
#   GO_ARCH:   Go GOARCH value                   (arm64   or amd64)
#   BSD_ARCH:  FreeBSD release path component    (arm64   or amd64)
case "$HOST_ARCH" in
    aarch64|arm64)
        RUST_ARCH="aarch64"
        GO_ARCH="arm64"
        BSD_ARCH="arm64"
        ;;
    x86_64|amd64)
        RUST_ARCH="x86_64"
        GO_ARCH="amd64"
        BSD_ARCH="amd64"
        ;;
    *)
        echo "Unsupported host architecture: $HOST_ARCH" >&2
        exit 1
        ;;
esac

VMPROXY_LINUX_TARGET="${RUST_ARCH}-unknown-linux-musl"
VMPROXY_BSD_TARGET="${RUST_ARCH}-unknown-freebsd"

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
        --config "target.${VMPROXY_LINUX_TARGET}.linker=\"clang\""
    )
    VMPROXY_BSD_LINKER_CFG=(
        --config "target.${VMPROXY_BSD_TARGET}.linker=\"clang\""
    )
    VMPROXY_LINUX_RUSTFLAGS_ENV=(
        env "RUSTFLAGS=-Clink-arg=--target=${VMPROXY_LINUX_TARGET} -Clink-arg=-fuse-ld=lld"
    )
    VMPROXY_BSD_RUSTFLAGS_ENV=(
        env "RUSTFLAGS=-Clink-arg=--target=${VMPROXY_BSD_TARGET} -Clink-arg=--sysroot=freebsd-sysroot -Clink-arg=-fuse-ld=lld -Clink-arg=-stdlib=libc++"
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

(cd "vmproxy" && "${VMPROXY_LINUX_RUSTFLAGS_ENV[@]}" cargo build --target "$VMPROXY_LINUX_TARGET" "${VMPROXY_LINUX_LINKER_CFG[@]}" $BUILD_ARGS $FEATURE_ARG)
mkdir -p libexec && cp "vmproxy/target/$VMPROXY_LINUX_TARGET/$BUILD_DIR/vmproxy" libexec/

(cd "vmrunner-sys" && cargo build $BUILD_ARGS)
cp "vmrunner-sys/target/$BUILD_DIR/libvmrunner_sys.a" "vmrunner-sys/target/"
(cd "init-rootfs" && go build -ldflags="-w -s" -tags containers_image_openpgp -o ../libexec/)

if [[ "$HOST_OS" == "Darwin" ]]; then
    codesign --entitlements "anylinuxfs.entitlements" --force -s - libexec/init-rootfs
fi

(cd "freebsd-bootstrap" && CGO_ENABLED=0 GOOS=freebsd GOARCH=$GO_ARCH go build -tags netgo -ldflags '-extldflags "-static" -w -s' -o ../libexec/)

SYSROOT=freebsd-sysroot
# x86_64-unknown-freebsd is a Tier 2 target with a precompiled std on stable;
# aarch64-unknown-freebsd is Tier 3 and needs nightly + -Z build-std.
if [[ "$RUST_ARCH" == "x86_64" ]]; then
    BSD_TOOLCHAIN_ARGS=()
    BSD_BUILD_STD_ARGS=()
else
    BSD_TOOLCHAIN_ARGS=(+nightly-2026-01-25)
    BSD_BUILD_STD_ARGS=(-Z build-std)
fi
(cd "vmproxy" \
    && test -d $SYSROOT \
    || (mkdir $SYSROOT && cd $SYSROOT \
        && curl -LO "http://ftp.cz.freebsd.org/pub/FreeBSD/releases/${BSD_ARCH}/14.3-RELEASE/base.txz" \
        && tar xJf base.txz 2>/dev/null || true && rm base.txz) \
    && "${VMPROXY_BSD_RUSTFLAGS_ENV[@]}" cargo "${BSD_TOOLCHAIN_ARGS[@]}" build "${VMPROXY_BSD_LINKER_CFG[@]}" "${BSD_BUILD_STD_ARGS[@]}" --target "$VMPROXY_BSD_TARGET" $BUILD_ARGS)
cp "vmproxy/target/$VMPROXY_BSD_TARGET/$BUILD_DIR/vmproxy" libexec/vmproxy-bsd
