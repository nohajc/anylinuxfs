# anylinuxfs
macOS: mount any linux-supported filesystem read/write using NFS and a microVM

## Introduction

Mounting 3rd-party filesystems on macOS has always been tricky. There's official support for reading NTFS but otherwise we've mainly used solutions based on macFUSE.
We've got NTFS-3g which is a pretty mature driver but for Linux filesystems there's only a couple of experimental solutions like [fuse-ext2](https://github.com/alperakcan/fuse-ext2) or [ext4fuse](https://github.com/gerard/ext4fuse).

If you want a reliable solution with write access, you need to run a Linux virtual machine with physical disk access and take care of exposing the mounted filesystem to the host.
This is exactly what `anylinuxfs` does and it streamlines it so that it's as easy as running one command in terminal.

You pick a drive, mount it with `anylinuxfs` and it appears as a NFS share on localhost. This spins up a microVM in the background which uses the real linux drivers, so you can access anything from `ext*` to `btrfs`. Any mount options on the command-line will be forwarded to the linux mount command, so you can mount read-only, read-write, pick btrfs subvolumes, etc. Then you simply eject the drive in Finder or use umount in terminal and the virtual machine will be turned off.

Thanks to the brilliant [libkrun](https://github.com/containers/libkrun), booting the Linux VM and starting NFS server only takes a couple of seconds, so the experience is pretty seamless. Current limitation is that only one anylinuxfs VM can run at a time.

## Installation
```
brew tap nohajc/anylinuxfs
brew install anylinuxfs
```

## Demo
https://github.com/user-attachments/assets/6ec6ce46-ce08-45b9-9fa4-e3d6d078d811
