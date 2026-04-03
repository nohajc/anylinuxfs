#!/bin/sh

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

CURL=/usr/bin/curl
TAR=/usr/bin/bsdtar
TRUNCATE=/usr/bin/truncate

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
BOOTSTRAP_IMAGE="freebsd-bootstrap.iso"
VM_DISK_IMAGE="freebsd-microvm-disk.img"

# 1. download oci image
if [ ! -f "$OCI_IMAGE" ]; then
    $CURL -LO "$OCI_IMAGE_URL"
fi
mkdir -p tmp/oci

# 2. unpack it
$TAR xf "$OCI_IMAGE" -C tmp/oci

# 3. repack as iso image
$TAR cf "$OCI_ISO_IMAGE" --format iso9660 --strip-components=2 tmp/oci

# 4. create bootstrap root image
mkdir -p tmp/rootfs/dev
mkdir -p tmp/rootfs/tmp

LIBEXEC_DIR="$SCRIPT_DIR/../libexec"
cp $LIBEXEC_DIR/freebsd-bootstrap tmp/rootfs/ # should be installed with the homebrew package
cp $LIBEXEC_DIR/init-freebsd tmp/rootfs/      # if this is shipped with the package, we can patch any existing VMs with it ASAP
cp $LIBEXEC_DIR/vmproxy-bsd tmp/rootfs/       # this will be installed with the homebrew package, new version should also be applied ASAP to all VMs

if [ ! -d "$SCRIPT_DIR/kernel" ]; then
    $CURL -LO "https://github.com/nohajc/freebsd/releases/download/alfs%2F14.3.0-p5/kernel.txz"
    $TAR xf kernel.txz -C "$SCRIPT_DIR"
    rm kernel.txz
fi

cp $SCRIPT_DIR/kernel/*.ko tmp/rootfs/       # kernel, modules and init can be installed under ~/.anylinuxfs but we always need to check if they're up to date
# ^ we must make sure modules are always in sync with the kernel, otherwise kldload will refuse to load them
# so, practically, any new version of kernel/modules should trigger re-creation of the entire microVM image
echo '{"iso_url": "'$ISO_IMAGE_URL'", "pkgs": ["bash", "pidof"]}' > tmp/rootfs/config.json

ENTRYPOINT_SH="entrypoint.sh"
if [ ! -f "$ENTRYPOINT_SH" ]; then
    $CURL -LO "https://raw.githubusercontent.com/nohajc/docker-nfs-server/refs/heads/freebsd/entrypoint.sh"
fi
chmod +x "$ENTRYPOINT_SH"
cp "$ENTRYPOINT_SH" tmp/rootfs/

$TAR cf "$BOOTSTRAP_IMAGE" --format iso9660 --strip-components=2 tmp/rootfs

# 5. create empty disk image
rm "$VM_DISK_IMAGE"
$TRUNCATE -s 8G "$VM_DISK_IMAGE"

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
    --set "vm.net.gvproxy_sock=$VFKIT_SOCK" \
    --set "command.path=/usr/local/bin/vm-setup.sh" \
    --set "command.args=nil"

kill $GVPROXY_PID
