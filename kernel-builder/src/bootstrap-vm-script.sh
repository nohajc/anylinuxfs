#!/bin/sh

set -e

RESOLV_CONF="nameserver 1.1.1.1"

echo "$RESOLV_CONF" > /etc/resolv.conf

apk add rsync skopeo umoci

IMAGE=debian
TAG=bookworm-slim

cd /root

# Cleanup from any previous run
rm -rf ${IMAGE}-${TAG} >/dev/null 2>&1 || true

# Download image using skopeo
echo "Downloading ${IMAGE}:${TAG} image..."
skopeo copy docker://${IMAGE}:${TAG} oci:${IMAGE}-${TAG}:latest

mount -t ext4 /dev/vda /mnt

while ! cat /init.krun > /mnt/init.krun; do
    echo "cat failed, retrying..."
    sleep 1
done
chmod +x /mnt/init.krun

MOUNT_POINT=/mnt
UMOCI_DST=$MOUNT_POINT/umoci
mkdir -p $UMOCI_DST

umoci unpack --image ${IMAGE}-${TAG}:latest $UMOCI_DST
rsync -au --remove-source-files $UMOCI_DST/rootfs/ $MOUNT_POINT/
rm -r $UMOCI_DST

echo "$RESOLV_CONF" > $MOUNT_POINT/etc/resolv.conf

# To avoid filesystem corruption
START_SHELL='trap "mount -o remount,ro /" EXIT; mount -t virtiofs shared /mnt; chronyd -q "server pool.ntp.org iburst"; bash -l'
echo "$START_SHELL" > $MOUNT_POINT/start-shell.sh
chmod +x $MOUNT_POINT/start-shell.sh

INSTALL_KERNEL_DEPS='trap "mount -o remount,ro /" EXIT; apt-get update && apt-get install -y curl build-essential python3-pyelftools bc kmod cpio flex libncurses5-dev libelf-dev libssl-dev dwarves bison chrony'
echo "$INSTALL_KERNEL_DEPS" > $MOUNT_POINT/install-kernel-deps.sh
chmod +x $MOUNT_POINT/install-kernel-deps.sh

umount /mnt
