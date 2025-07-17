#!/bin/bash

set -e

SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

IMAGE_ARCHIVE_NAME="linux-aarch64-Image-v6.12.34-anylinuxfs.tar.gz"
IMAGE_ARCHIVE_URL="https://github.com/nohajc/libkrunfw/releases/download/v6.12.34-rev1/${IMAGE_ARCHIVE_NAME}"

GVPROXY_VERSION="0.8.6"
GVPROXY_URL="https://github.com/containers/gvisor-tap-vsock/releases/download/v${GVPROXY_VERSION}/gvproxy-darwin"

cd "$SCRIPT_DIR"
curl -L -o "$IMAGE_ARCHIVE_NAME" "$IMAGE_ARCHIVE_URL"
mkdir -p "libexec"
tar xzf "$IMAGE_ARCHIVE_NAME" -C "libexec"
rm "$IMAGE_ARCHIVE_NAME"

curl -L -o libexec/gvproxy "$GVPROXY_URL"
chmod +x libexec/gvproxy
