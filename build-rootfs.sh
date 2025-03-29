#!/bin/sh

SCRIPT_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )

cd "$SCRIPT_DIR"
mkdir -p tmp
cd tmp

rm -rf docker-nfs-server || true
git clone https://github.com/nohajc/docker-nfs-server.git
cd docker-nfs-server

IMAGE_NAME=nfs-server-alpine
ROOTFS_DIR=../../vmroot

mkdir -p "$ROOTFS_DIR"

podman build -t "$IMAGE_NAME" .
podman create --name "$IMAGE_NAME" "$IMAGE_NAME"
podman export "$IMAGE_NAME" | tar xpf - -C "$ROOTFS_DIR"
podman rm "$IMAGE_NAME"
