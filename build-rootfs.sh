#!/bin/sh

SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

cd "$SCRIPT_DIR"
mkdir -p tmp
cd tmp

rm -rf docker-nfs-server || true
git clone https://github.com/nohajc/docker-nfs-server.git
cd docker-nfs-server

IMAGE_NAME=nfs-server-alpine
ROOTFS_DIR=../../bin/vmroot

mkdir -p "$ROOTFS_DIR"

podman build -t "$IMAGE_NAME" .
podman create --name "$IMAGE_NAME" "$IMAGE_NAME"
podman export "$IMAGE_NAME" | tar xpf - -C "$ROOTFS_DIR"
podman rm "$IMAGE_NAME"

echo '/mnt/hostblk      *(ro,no_subtree_check,no_root_squash,insecure)' > "$ROOTFS_DIR/etc/exports"
echo 'nameserver 192.168.1.111' > "$ROOTFS_DIR/etc/resolv.conf"

cat << EOF > "$ROOTFS_DIR/init-network.sh"
ip addr add 192.168.127.2/24 dev eth0
ip link set eth0 up
ip route add default via 192.168.127.1 dev eth0
curl http://192.168.127.1/services/forwarder/expose -X POST -d '{"local":":111","remote":"192.168.127.2:111"}'
curl http://192.168.127.1/services/forwarder/expose -X POST -d '{"local":"127.0.0.1:2049","remote":"192.168.127.2:2049"}'
curl http://192.168.127.1/services/forwarder/expose -X POST -d '{"local":"127.0.0.1:32765","remote":"192.168.127.2:32765"}'
curl http://192.168.127.1/services/forwarder/expose -X POST -d '{"local":"127.0.0.1:32767","remote":"192.168.127.2:32767"}'
EOF

chmod +x "$ROOTFS_DIR/init-network.sh"
