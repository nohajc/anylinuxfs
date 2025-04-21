# anylinuxfs
macOS: mount any linux-supported filesystem read/write using NFS and a microVM

## Installation
```
brew tap nohajc/anylinuxfs
brew install anylinuxfs
```

## Demo
https://github.com/user-attachments/assets/6ec6ce46-ce08-45b9-9fa4-e3d6d078d811

## Introduction

Mounting 3rd-party filesystems on macOS has always been tricky. There's official support for reading NTFS but otherwise we've mainly used solutions based on macFUSE.
We've got NTFS-3g which is a pretty mature driver but for Linux filesystems there's only a couple of experimental solutions like [fuse-ext2](https://github.com/alperakcan/fuse-ext2) or [ext4fuse](https://github.com/gerard/ext4fuse).

If you want a reliable solution with write access, you need to run a Linux virtual machine with physical disk access and take care of exposing the mounted filesystem to the host.
This is exactly what `anylinuxfs` does and it streamlines it so that it's as easy as running one command in terminal.

You pick a drive, mount it with `anylinuxfs` and it appears as a NFS share on localhost. This spins up a microVM in the background which uses the real linux drivers, so you can access anything from `ext*` to `btrfs`. Any mount options on the command-line will be forwarded to the linux mount command, so you can mount read-only, read-write, pick btrfs subvolumes, etc. Then you simply eject the drive in Finder or use `umount` in terminal and the virtual machine will be turned off.

This all sounds like a lot of work but it's actually very fast. Not like a traditional virtual machine which takes a while to boot.
This one is just a stripped down version of Linux, ther's not even a UEFI firmware. Practically, it takes only a couple of seconds before the drive is mounted and ready to use.

## Notes

When you first run `anylinuxfs` to mount a drive, it will download the alpine Linux image from Docker hub and unpack it to your user profile (`~/.anylinuxfs/alpine`).
Then it will spin up a VM so it can install dependencies and do the initial environment setup. After that, the Linux root filesystem will be reused for every mount operation.
You can also run `anylinuxfs init` to download a fresh copy of `alpine:latest` and reinitialize the environment at any time.

## Limitations
- Only one drive can be mounted at a time (this might be improved in the future)
- Only Apple Silicon Macs are supported (libkrun limitation)

## Acknowledgements
This project was made possible by
- [libkrun](https://github.com/containers/libkrun) the microVM hypervisor for Linux anc Mac
- [libkrunfw](https://github.com/containers/libkrunfw) - Linux kernel bundled for libkrun as a dynamic library
- [gvproxy](https://github.com/containers/gvisor-tap-vsock) - user space networking for virtual machines (also used by podman)
- [docker-nfs-server](https://github.com/ehough/docker-nfs-server) - launcher for NFS server in a container

Thank you all for your great work!
