#!/bin/sh

set -e

BOOTSTRAP_IMAGE=alpine
BOOTSTRAP_TAG=latest

# Cleanup from any previous run
rm -rf ${BOOTSTRAP_IMAGE}-${BOOTSTRAP_TAG} >/dev/null 2>&1 || true

# Download image using skopeo
echo "Downloading ${BOOTSTRAP_IMAGE}:${BOOTSTRAP_TAG} image..."
skopeo copy --override-os linux docker://${BOOTSTRAP_IMAGE}:${BOOTSTRAP_TAG} oci:${BOOTSTRAP_IMAGE}-${BOOTSTRAP_TAG}:latest

umoci unpack --rootless --image ${BOOTSTRAP_IMAGE}-${BOOTSTRAP_TAG}:latest ${BOOTSTRAP_IMAGE}-${BOOTSTRAP_TAG}

export PATH="/opt/homebrew/opt/e2fsprogs/sbin:$PATH"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

cp $SCRIPT_DIR/src/bootstrap-vm-script.sh ${BOOTSTRAP_IMAGE}-${BOOTSTRAP_TAG}/rootfs/

IMAGE=debian
TAG=bookworm-slim

# Create disk image
truncate -s 1G ${IMAGE}-${TAG}.img
mkfs.ext4 -L ${IMAGE} -F ${IMAGE}-${TAG}.img

$SCRIPT_DIR/src/vm-run.lua --config $SCRIPT_DIR/src/bootstrap-vm-config.lua
$SCRIPT_DIR/src/vm-run.lua --config $SCRIPT_DIR/src/debian-vm-config.lua --set 'command.args[1]=/install-kernel-deps.sh'
