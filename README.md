<picture>
   <source media="(prefers-color-scheme: dark)" srcset="https://github.com/user-attachments/assets/352661ce-c05a-4454-8087-0c489139eb36">
   <source media="(prefers-color-scheme: light)" srcset="https://github.com/user-attachments/assets/8b78948d-3751-4b41-a9f1-6fc85afb4121">
   <img alt="anylinuxfs logo" src="https://github.com/user-attachments/assets/8b78948d-3751-4b41-a9f1-6fc85afb4121">
</picture>

An easy way to mount ext4, btrfs or in fact any linux-supported filesystem on a Mac.
With full write support, based on the libkrun microVM hypervisor and NFS. Doesn't require installing any kernel extensions or lowering system security.

<a href='https://ko-fi.com/Q5Q41EHAGK' target='_blank'><img height='36' style='border:0px;height:36px;' src='https://storage.ko-fi.com/cdn/kofi1.png?v=6' border='0' alt='Buy Me a Coffee at ko-fi.com' /></a>

## Installation
```
brew tap nohajc/anylinuxfs
brew install anylinuxfs
```

> [!TIP]
> This is a CLI utility. If you prefer GUI, see [anylinuxfs-gui](https://github.com/fenio/anylinuxfs-gui).

## Features
- mounts any filesystem supported by Linux (**ext4**, **btrfs**, **xfs**, ... but also **NTFS** and **exFAT**)
- mount any number of drives at the same time
- supports **LUKS**-encrypted drives
- supports **BitLocker**-encrypted drives – **NTFS** or **FAT32** (using your recovery key as passphrase)
- supports **LVM** (even volume groups spanning multiple drives)
- supports **LVM on LUKS** (i.e. encrypted LVM)
- supports **Linux RAID** (mdadm) and **multi-disk btrfs**
- supports **ZFS** (including native ZFS encryption)
- works with both external and internal drives
- works with disk images
- supports disks with **GPT**, **MBR** or no partition table (single filesystem or LVM/LUKS container)
- NFS share by default only reachable from localhost but can be shared across network too
- define your own [custom actions](docs/custom-actions.md) (e.g. mount **borg backup** located on a Linux drive)

## Limitations
- Only Apple Silicon Macs are supported (libkrun limitation)
- A new virtual machine is needed for each mounted volume.
  You can configure RAM limit (512 MB by default), typical usage is around 256 MB per VM.
- Multi-mount relies on file locks (to prevent data corruption) and we can only lock entire physical partitions.
  So, practically, you won't be able to mount multiple logical volumes at once if they're backed by the same partition.
- Some disks with non-ASCII labels might fail to mount unless a custom mount point with "safe" characters is used (this is a macOS bug)
- By design, any mounted volume is seen by macOS as a network drive shared by our virtual machine.
- Microsoft Word cannot edit files directly on a mounted drive. This is a known issue with Word and network filesystems.

> [!CAUTION]
> Before using anylinuxfs with **NTFS**, please read [the notes](docs/important-notes.md#ntfs)

## Demo
https://github.com/user-attachments/assets/6ec6ce46-ce08-45b9-9fa4-e3d6d078d811

## Introduction

Mounting 3rd-party filesystems on macOS has always been tricky. There's official support for reading NTFS but otherwise we've mainly used solutions based on macFUSE.
We've got NTFS-3g which is a pretty mature driver but for Linux filesystems there's only a couple of experimental solutions like [fuse-ext2](https://github.com/alperakcan/fuse-ext2) or [ext4fuse](https://github.com/gerard/ext4fuse).

If you want a reliable solution with write access, you need to run a Linux virtual machine with physical disk access and take care of exposing the mounted filesystem to the host.
This is exactly what `anylinuxfs` does and it streamlines it so that it's as easy as running one command in terminal.

You pick a drive, mount it with `anylinuxfs` and it appears as a NFS share on localhost. This spins up a microVM in the background which uses the real linux drivers, so you can access anything from `ext*` to `btrfs`. Any mount options on the command-line will be forwarded to the linux mount command, so you can mount read-only, read-write, pick btrfs subvolumes, etc. Then you simply eject the drive in Finder or use `anylinuxfs unmount` in terminal and the virtual machine will be turned off.

This all sounds like a lot of work but it's actually very fast. Not like a traditional virtual machine which takes a while to boot.
This one is just a stripped down version of Linux, there's not even a UEFI firmware. Practically, it takes only a couple of seconds before the drive is mounted and ready to use.

## Basic usage

Most often, you will probably use the following commands:
* `anylinuxfs mount` - mount a filesystem; this is the default command, so the `mount` keyword can be omitted
* `anylinuxfs unmount` - safe unmount, useful in case of multiple mounts (typically ZFS datasets) which need to be ejected in a particular order
* `anylinuxfs list` - show available filesystems (`-m`/`-l` shows Microsoft/Linux partitions only)
* `anylinuxfs status` - show what is currently mounted
* `anylinuxfs log` - show details about the current (or last) run, useful for troubleshooting

### Mounting filesystems

From `anylinuxfs mount --help`:
```
Usage: anylinuxfs [mount] [OPTIONS] [DISK_IDENT] [MOUNT_POINT]

Arguments:
  [DISK_IDENT]   File path(s), LVM identifier or RAID identifier, e.g.:
                 /dev/diskXsY[:/dev/diskYsZ:...]
                 lvm:<vg-name>:diskXsY[:diskYsZ:...]:<lv-name>
                 raid:diskXsY[:diskYsZ:...]
                 (see `list` command output for available volumes)
  [MOUNT_POINT]  Custom mount path to override the default under /Volumes
```

* The only argument you usually have to provide is the disk identifier (unless you use a "diskless" custom action).
* It must always refer to one or more partitions or logical volumes (not whole disks).
* Basic syntax of an identifier is `/dev/diskXsY` - based on how `anylinuxfs list` or `diskutil list` identifies your drives.
* If your filesystem is on a logical volume, you will usually need a special prefixed identifier starting with `lvm` or `raid` (for mdadm Linux RAID).
  These can be deduced from `anylinuxfs list` output where any logical volumes will be shown as synthesized disks (similar to how `diskutil` does it for APFS containers)
* In case of btrfs filesystems spanning multiple disks (like RAID1 or JBOD), these will not be grouped in the `anylinuxfs list` output.
* In order to mount a filesystem like this, you use the `/dev/diskXsY:/dev/diskYsZ` syntax. Basically, you must specify all partitions that need to be attached to our microVM so that they can be scanned for any multi-disk btrfs filesystems.

## Documentation

- [Examples](docs/examples.md) — command examples for common use cases
- [Custom actions](docs/custom-actions.md) — scripting hooks that run inside the VM
- [Important notes](docs/important-notes.md) — VM initialization, permissions, LUKS memory, NTFS drivers
- [Troubleshooting](docs/troubleshooting.md) — common issues and fixes
- [Build from source](docs/building.md) — how to build anylinuxfs yourself

## Acknowledgements
Logo designed by [Junaid Iqbal](https://www.fiverr.com/junaidiqbal39).

This project was made possible by
- [libkrun](https://github.com/containers/libkrun) the microVM hypervisor for Linux and Mac
- [libkrunfw](https://github.com/containers/libkrunfw) - Linux kernel bundled for libkrun as a dynamic library
- [gvproxy](https://github.com/containers/gvisor-tap-vsock) - user space networking for virtual machines (also used by podman)
- [docker-nfs-server](https://github.com/ehough/docker-nfs-server) - launcher for NFS server in a container

Thank you all for your great work!
