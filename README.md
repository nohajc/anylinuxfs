# anylinuxfs
macOS: mount any linux-supported filesystem read/write using NFS and a microVM

<a href='https://ko-fi.com/Q5Q41EHAGK' target='_blank'><img height='36' style='border:0px;height:36px;' src='https://storage.ko-fi.com/cdn/kofi1.png?v=6' border='0' alt='Buy Me a Coffee at ko-fi.com' /></a>

## Features
- mounts any filesystem supported by Linux
- supports LUKS-encrypted drives
- supports LVM (even volume groups spanning multiple drives)
- supports LVM on LUKS (i.e. encrypted LVM)
- supports disks with GPT, MBR or no partition table (single filesystem or LVM/LUKS container)
- NFS share by default only reachable from localhost but can be shared across network too

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
This one is just a stripped down version of Linux, there's not even a UEFI firmware. Practically, it takes only a couple of seconds before the drive is mounted and ready to use.

## Notes

- When you first run `anylinuxfs` to mount a drive, it will download the alpine Linux image from Docker hub and unpack it to your user profile (`~/.anylinuxfs/alpine`).
Then it will spin up a VM so it can install dependencies and do the initial environment setup. After that, the Linux root filesystem will be reused for every mount operation.
You can also run `anylinuxfs init` to download a fresh copy of `alpine:latest` and reinitialize the environment at any time.
- It is needed to run mount commands with `sudo` otherwise we're not allowed direct access to `/dev/disk*` files. However, the virtual machine itself will in fact run under the regular user who invoked `sudo` in the first place (i.e. all unnecessary permissions are dropped after the disk is opened)
- When you mount a LUKS-encrypted drive, the microVM requires at least 2.5 GiB RAM for cryptsetup to work properly. If your VM is configured with a lower amount of memory, you'll get a warning about it and the RAM configuration will be adjusted automatically. If you don't want to see the warning, set your default RAM to match this requirement (`anylinuxfs config -r <size-in-MiB>`)
- Configured amount of RAM is the maximum that can be allocated. The actual amount of memory consumed by the VM can be lower.

## Limitations
- Only one drive can be mounted at a time (this might be improved in the future)
- Only Apple Silicon Macs are supported (libkrun limitation)

## Troubleshooting
- Check your mount flags (e.g. the `subvol` flag from demo is specific to btrfs, make sure you don't use it with other filesystems)
- Check file owner and permissions with `ls -l` and adjust accordingly. Typically, your macOS user won't have write access to your drive out of the box so you need to write files as root or first prepare a target directory writable by everyone (`chmod 777`).
- If you get `fcopyfile failed: Operation not permitted`, it can actually mean the file you're trying to copy has the quarantine attribute set (can be removed with `xattr -d com.apple.quarantine <filename>`)
- Accessing disks might require Full Disk Access permission (although you should get pop-ups that let you allow access case-by-case)

## Acknowledgements
This project was made possible by
- [libkrun](https://github.com/containers/libkrun) the microVM hypervisor for Linux anc Mac
- [libkrunfw](https://github.com/containers/libkrunfw) - Linux kernel bundled for libkrun as a dynamic library
- [gvproxy](https://github.com/containers/gvisor-tap-vsock) - user space networking for virtual machines (also used by podman)
- [docker-nfs-server](https://github.com/ehough/docker-nfs-server) - launcher for NFS server in a container

Thank you all for your great work!
