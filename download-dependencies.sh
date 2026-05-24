#!/bin/bash

set -e

SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

HOST_OS="$(uname -s)"

IMAGE_ARCHIVE_NAME="linux-aarch64-Images-v6.12.62-anylinuxfs.tar.gz"
RELEASE_URL="https://github.com/nohajc/libkrunfw/releases/download/v6.12.62-rev1"
IMAGE_ARCHIVE_URL="${RELEASE_URL}/${IMAGE_ARCHIVE_NAME}"
MODULES_ARCHIVE_NAME="modules.squashfs"
MODULES_ARCHIVE_URL="${RELEASE_URL}/${MODULES_ARCHIVE_NAME}"

INIT_BSD="init-freebsd"
INIT_BSD_URL="https://github.com/nohajc/libkrun/releases/download/v1.17.0-init-bsd/${INIT_BSD}"

if [[ "$HOST_OS" == "Darwin" ]]; then
    GVPROXY_VERSION="0.8.8"
    GVPROXY_URL="https://github.com/containers/gvisor-tap-vsock/releases/download/v${GVPROXY_VERSION}/gvproxy-darwin"

    VMNET_HELPER_VERSION="0.12.0"
    VMNET_HELPER_URL="https://github.com/nirs/vmnet-helper/releases/download/v${VMNET_HELPER_VERSION}/vmnet-helper.tar.gz"
fi

cd "$SCRIPT_DIR"
curl -L -o "$IMAGE_ARCHIVE_NAME" "$IMAGE_ARCHIVE_URL"
mkdir -p "libexec"
tar xzf "$IMAGE_ARCHIVE_NAME" -C "libexec"
rm "$IMAGE_ARCHIVE_NAME"

curl -LO "$MODULES_ARCHIVE_URL"
mkdir -p "lib"
mv ${MODULES_ARCHIVE_NAME} lib/

curl -LO "$INIT_BSD_URL"
mv "$INIT_BSD" "libexec/"
chmod +x "libexec/$INIT_BSD"

if [[ "$HOST_OS" == "Darwin" ]]; then
    curl -L -o libexec/gvproxy "$GVPROXY_URL"
    chmod +x libexec/gvproxy
else
    # Build gvproxy from a pinned commit on upstream. The vfkit-mode patch we
    # needed is merged but not in a tagged release yet; switch back to a
    # tagged tarball once a release ships.
    GVPROXY_COMMIT="c09fb7d0a9e08260d238066c3c4311498d15311e"
    GVPROXY_TMP="$(mktemp -d "$SCRIPT_DIR/.gvproxy-build.XXXXXX")"
    git -C "$GVPROXY_TMP" init -q
    git -C "$GVPROXY_TMP" remote add origin https://github.com/containers/gvisor-tap-vsock.git
    git -C "$GVPROXY_TMP" fetch --depth=1 origin "$GVPROXY_COMMIT"
    git -C "$GVPROXY_TMP" checkout -q FETCH_HEAD
    (cd "$GVPROXY_TMP" && make)
    cp "$GVPROXY_TMP/bin/gvproxy" libexec/gvproxy
    chmod +x libexec/gvproxy
    rm -rf "$GVPROXY_TMP"
fi

if [[ "$HOST_OS" == "Darwin" ]]; then
    curl -LO "$VMNET_HELPER_URL"
    tar xzf vmnet-helper.tar.gz -C libexec --strip-components=4 ./opt/vmnet-helper/bin/vmnet-helper
    rm vmnet-helper.tar.gz
fi
