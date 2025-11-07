#!/bin/sh

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

CURL=/usr/bin/curl
TAR=/usr/bin/bsdtar

# http://ftp.cz.freebsd.org/pub/FreeBSD/releases/ISO-IMAGES/14.3/FreeBSD-14.3-RELEASE-arm64-aarch64-bootonly.iso
# http://ftp.cz.freebsd.org/pub/FreeBSD/releases/OCI-IMAGES/14.3-RELEASE/aarch64/Latest/FreeBSD-14.3-RELEASE-arm64-aarch64-container-image-runtime.txz

BSD_RELEASE_VERSION="14.3"
BSD_RELEASE_SUFFIX="RELEASE"
# BSD_RELEASE_VERSION="15.0"
# BSD_RELEASE_SUFFIX="BETA4"
BSD_RELEASE_NAME="FreeBSD-$BSD_RELEASE_VERSION-$BSD_RELEASE_SUFFIX-arm64-aarch64"

ISO_IMAGE_URL="http://ftp.cz.freebsd.org/pub/FreeBSD/releases/ISO-IMAGES/$BSD_RELEASE_VERSION/$BSD_RELEASE_NAME-bootonly.iso"

OCI_IMAGE="$BSD_RELEASE_NAME-container-image-runtime.txz"
OCI_IMAGE_URL="http://ftp.cz.freebsd.org/pub/FreeBSD/releases/OCI-IMAGES/$BSD_RELEASE_VERSION-$BSD_RELEASE_SUFFIX/aarch64/Latest/$OCI_IMAGE"

OCI_ISO_IMAGE="freebsd-oci.iso"
ROOTFS_IMAGE="freebsd-bootstrap.iso"
VM_DISK_IMAGE="freebsd-microvm-disk.img"

# 1. download oci image
$CURL -LO "$OCI_IMAGE_URL"
mkdir -p tmp/oci

# 2. unpack it
$TAR xf "$OCI_IMAGE" -C tmp/oci

# 3. repack as iso image
$TAR cf "$OCI_ISO_IMAGE" --format iso9660 --strip-components=2 tmp/oci

# 4. create bootstrap root image
mkdir -p tmp/rootfs/dev
mkdir -p tmp/rootfs/tmp
cp $SCRIPT_DIR/../freebsd-bootstrap/freebsd-bootstrap tmp/rootfs/
cp $SCRIPT_DIR/freebsd/init-freebsd tmp/rootfs/
echo '{"iso_url": "'$ISO_IMAGE_URL'"}' > tmp/rootfs/config.json

$TAR cf "$ROOTFS_IMAGE" --format iso9660 --strip-components=2 tmp/rootfs

# 5. create empty disk image
rm "$VM_DISK_IMAGE"
truncate -s 8G "$VM_DISK_IMAGE"

# 6. boot the vm
VFKIT_SOCK="vfkit-sock"

gvproxy --listen unix://$PWD/network.sock --listen-vfkit unixgram://$PWD/$VFKIT_SOCK --ssh-port -1 2>/dev/null &
GVPROXY_PID=$!
trap "rm -rf tmp; rm $VFKIT_SOCK-krun.sock" EXIT

$SCRIPT_DIR/src/vm-run.lua --config $SCRIPT_DIR/src/freebsd-bootstrap-vm-config.lua \
    --set "vm.net.gvproxy_sock=$VFKIT_SOCK"

kill $GVPROXY_PID

gvproxy --listen unix://$PWD/network.sock --listen-vfkit unixgram://$PWD/$VFKIT_SOCK --ssh-port -1 2>/dev/null &
GVPROXY_PID=$!

$SCRIPT_DIR/src/vm-run.lua --config $SCRIPT_DIR/src/freebsd-microvm-config.lua \
    --set "vm.net.gvproxy_sock=$VFKIT_SOCK"

kill $GVPROXY_PID
