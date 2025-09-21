#!/bin/bash

set -e

SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

IMAGE_ARCHIVE_NAME="linux-aarch64-Image-v6.12.34-anylinuxfs.tar.gz"
RELEASE_URL="https://github.com/nohajc/libkrunfw/releases/download/v6.12.34-rev4"
IMAGE_ARCHIVE_URL="${RELEASE_URL}/${IMAGE_ARCHIVE_NAME}"
MODULES_ARCHIVE_NAME="modules.squashfs"
MODULES_ARCHIVE_URL="${RELEASE_URL}/${MODULES_ARCHIVE_NAME}"

GVPROXY_VERSION="0.8.7"
GVPROXY_URL="https://github.com/containers/gvisor-tap-vsock/releases/download/v${GVPROXY_VERSION}/gvproxy-darwin"

cd "$SCRIPT_DIR"
curl -L -o "$IMAGE_ARCHIVE_NAME" "$IMAGE_ARCHIVE_URL"
mkdir -p "libexec"
tar xzf "$IMAGE_ARCHIVE_NAME" -C "libexec"
rm "$IMAGE_ARCHIVE_NAME"

curl -LO "$MODULES_ARCHIVE_URL"
mkdir -p "lib"
mv ${MODULES_ARCHIVE_NAME} lib/

curl -L -o libexec/gvproxy "$GVPROXY_URL"
chmod +x libexec/gvproxy
