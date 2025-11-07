#!/bin/sh

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

CURL=/usr/bin/curl
TAR=/usr/bin/bsdtar

OCI_IMAGE="FreeBSD-14.3-RELEASE-arm64-aarch64-container-image-runtime.txz"
OCI_IMAGE_URL="https://download.freebsd.org/releases/OCI-IMAGES/14.3-RELEASE/aarch64/Latest/$OCI_IMAGE"

ISO_IMAGE="freebsd-oci.iso"
ROOTFS_IMAGE="freebsd-bootstrap.iso"
VM_DISK_IMAGE="freebsd-microvm-disk.img"

# 1. download oci image
$CURL -LO "$OCI_IMAGE_URL"
mkdir -p tmp/oci
trap 'rm -rf tmp' EXIT

# 2. unpack it
$TAR xf "$OCI_IMAGE" -C tmp/oci

# 3. repack as iso image
$TAR cf "$ISO_IMAGE" --format iso9660 --strip-components=2 tmp/oci

# 4. create bootstrap root image
mkdir -p tmp/rootfs/dev
mkdir -p tmp/rootfs/tmp
cp $SCRIPT_DIR/../freebsd-bootstrap/freebsd-bootstrap tmp/rootfs/
cp $SCRIPT_DIR/freebsd/init-freebsd tmp/rootfs/

$TAR cf "$ROOTFS_IMAGE" --format iso9660 --strip-components=2 tmp/rootfs

# 5. create empty disk image
rm "$VM_DISK_IMAGE"
truncate -s 8G "$VM_DISK_IMAGE"

# 6. boot the vm
VFKIT_SOCK="vfkit-sock"

gvproxy --listen unix://$PWD/network.sock --listen-vfkit unixgram://$PWD/$VFKIT_SOCK --ssh-port -1 2>/dev/null &
GVPROXY_PID=$!
trap "kill $GVPROXY_PID; rm $VFKIT_SOCK-krun.sock" EXIT

$SCRIPT_DIR/src/vm-run.lua --config $SCRIPT_DIR/src/freebsd-bootstrap-vm-config.lua \
    --set "vm.net.gvproxy_sock=$VFKIT_SOCK"
