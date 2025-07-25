# anylinuxfs
An easy way to mount ext4, btrfs or in fact any linux-supported filesystem on a Mac.
With full write support, based on the libkrun microVM hypervisor and NFS. Doesn't require installing any kernel extensions or lowering system security.

<a href='https://ko-fi.com/Q5Q41EHAGK' target='_blank'><img height='36' style='border:0px;height:36px;' src='https://storage.ko-fi.com/cdn/kofi1.png?v=6' border='0' alt='Buy Me a Coffee at ko-fi.com' /></a>

## Features
- mounts any filesystem supported by Linux (**ext4**, **btrfs**, **xfs**, ... but also **NTFS** and **exFAT**)
- supports **LUKS**-encrypted drives
- supports **BitLocker**-encrypted drives (using your recovery key as passphrase)
- supports **LVM** (even volume groups spanning multiple drives)
- supports **LVM on LUKS** (i.e. encrypted LVM)
- supports **Linux RAID**
- works with both external and internal drives
- works with disk images
- supports disks with **GPT**, **MBR** or no partition table (single filesystem or LVM/LUKS container)
- NFS share by default only reachable from localhost but can be shared across network too

> [!CAUTION]
> Before using anylinuxfs with **NTFS**, please read [the notes](#ntfs)

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

## Examples

### List available drives with Linux filesystems
```
sudo anylinuxfs list
```
**Possible output**
```
/dev/disk0 (internal, physical):
   #:                       TYPE NAME                    SIZE       IDENTIFIER
   0:      GUID_partition_scheme                        *500.3 GB   disk0
   5:                       ext4 BOOT                    1.0 GB     disk0s5
   6:                      btrfs fedora                  144.2 GB   disk0s6

/dev/disk7 (external, physical):
   #:                       TYPE NAME                    SIZE       IDENTIFIER
   0:     FDisk_partition_scheme                        *30.8 GB    disk7
   1:                LVM2_member                         30.8 GB    disk7s1

/dev/disk8 (external, physical):
   #:                       TYPE NAME                    SIZE       IDENTIFIER
   0:      GUID_partition_scheme                        *4.2 GB     disk8
   1:                LVM2_member                         4.2 GB     disk8s1

/dev/disk9 (external, physical):
   #:                       TYPE NAME                    SIZE       IDENTIFIER
   0:                crypto_LUKS                        *8.1 GB     disk9

/dev/disk10 (disk image):
   #:                       TYPE NAME                    SIZE       IDENTIFIER
   0:     FDisk_partition_scheme                        +268.4 MB   disk10
   1:          linux_raid_member debian:0                267.4 MB   disk10s1

/dev/disk11 (disk image):
   #:                       TYPE NAME                    SIZE       IDENTIFIER
   0:      GUID_partition_scheme                        +268.4 MB   disk11
   1:          linux_raid_member debian:0                266.3 MB   disk11s1

raid:disk10s1:disk11s1 (volume):
   #:                       TYPE NAME                    SIZE       IDENTIFIER
   0:                       ext4 raid-test               265.3 MB   disk10s1:disk11s1

lvm:vg1 (volume group):
   #:                       TYPE NAME                    SIZE       IDENTIFIER
   0:                LVM2_scheme                        +35.0 GB    vg1
                                 Physical Store disk7s1
                                                disk8s1
   1:                       ext4 lvol0                   15.4 GB    vg1:disk7s1:lvol0
   2:                        xfs lvol1                   7.7 GB     vg1:disk7s1:lvol1
   3:                      btrfs lvol2                   11.9 GB    vg1:disk7s1:disk8s1:lvol2
```

### List available drives with Microsoft filesystems (exFAT, NTFS)
```
sudo anylinuxfs list -m
```

### Mount partition read/write
```
sudo anylinuxfs /dev/disk0s6
```

### Mount partition read-only
```
sudo anylinuxfs /dev/disk0s6 -o ro
```

### Mount logical volume from group vg1 backed by disk7s1
```
sudo anylinuxfs lvm:vg1:disk7s1:lvol0
```

### Mount logical volume from group vg1 backed by disk7s1 and disk8s1
```
sudo anylinuxfs lvm:vg1:disk7s1:disk8s1:lvol2
```

### Mount RAID volume backed by disk10s1 and disk11s1
```
sudo anylinuxfs raid:disk10s1:disk11s1
```

### List available drives and decrypt LUKS or BitLocker metadata of disk9
```
sudo anylinuxfs list -d /dev/disk9
```
**Output will show the encrypted partition filesystem and label**
```
...
/dev/disk9 (external, physical):
   #:                       TYPE NAME                    SIZE       IDENTIFIER
   0:          crypto_LUKS: ext4 enc-ext4               *8.1 GB     disk9
...
```

### List available drives and decrypt all LUKS or BitLocker metadata
```
sudo anylinuxfs list -d all
```

### Mount LUKS-encrypted or BitLocker-encrypted partition
```
sudo anylinuxfs /dev/disk9
```

### Mount partition and share it via NFS to other devices in any subnet
```
sudo anylinuxfs /dev/disk0s6 -b 0.0.0.0
```

### Mount partition and share it via NFS to devices within your subnet (more secure)
```
# by server, we mean the device which is sharing the mounted filesystem
sudo anylinuxfs /dev/disk0s6 -b <YOUR_SERVER_IP>
```

### Show current mount status
```
anylinuxfs status
```

### Try to stop anylinuxfs in case umount or eject didn't completely terminate the VM
```
anylinuxfs stop
```

## Notes

### VM initialization
- When you first run `anylinuxfs` to mount a drive, it will download the alpine Linux image from Docker hub and unpack it to your user profile (`~/.anylinuxfs/alpine`).
Then it will spin up a VM so it can install dependencies and do the initial environment setup. After that, the Linux root filesystem will be reused for every mount operation.
You can also run `anylinuxfs init` to download a fresh copy of `alpine:latest` and reinitialize the environment at any time.

### Permissions
- It is needed to run mount commands with `sudo` otherwise we're not allowed direct access to `/dev/disk*` files. However, the virtual machine itself will in fact run under the regular user who invoked `sudo` in the first place (i.e. all unnecessary permissions are dropped after the disk is opened)

### Memory requirements for LUKS
- When you mount a LUKS-encrypted drive, the microVM requires at least 2.5 GiB RAM for cryptsetup to work properly. If your VM is configured with a lower amount of memory, you'll get a warning about it and the RAM configuration will be adjusted automatically. If you don't want to see the warning, set your default RAM to match this requirement (`anylinuxfs config -r <size-in-MiB>`)
- Configured amount of RAM is the maximum that can be allocated. The actual amount of memory consumed by the VM can be lower.

### NTFS
* anylinuxfs provides two different NTFS drivers
  - the user-space FUSE-based **ntfs-3g** (better compatibility)
  - the more recent kernel-space **ntfs3** (significantly better performance)
* **ntfs-3g** is used by default
* **ntfs3** can be used by specifying `-t ntfs3` option when mounting
* Important things to keep in mind
  - **ntfs3** cannot mount NTFS drives from Windows systems which were hibernated or which have Fast Startup enabled
  - **ntfs-3g** will fall back to read-only mount and issue a warning in this case
  - **ntfs3** will generally refuse to mount a drive if it has any filesystem errors
  - using any unofficial tools like `ntfsfix` to clear dirty flag will not really fix those errors and can lead to further data corruption!
  - `chkdsk` on Windows is the recommended way to fix NTFS errors
  - some users also have good experience with [`chkntfs`](https://gist.github.com/nohajc/51fdecb2dda75dd8c600173ea42b3700) by Paragon (proprietary)
  - there are permission issues reported when using **ntfs3** with Windows system drives
  - specifically, `/Program Files` some folders within `/Users` are read-only (see this [reddit](https://www.reddit.com/r/archlinux/comments/r325t3/permissions_problems_with_the_new_ntfs3_driver/) post for details)

#### To summarize
* There are stories online about data corruption (or system freezes) caused by the **ntfs3** driver.
* They might or might not be caused by improper use of `ntfsfix`.
* **ntfs3** is included in the mainline Linux kernel so it is considered stable. It was contributed by Paragon Software in [2021](https://www.paragon-software.com/paragon-software-announces-the-inclusion-of-its-ntfs3-driver-into-linux-kernel-5-15/).
* If you trust it, want the best performance and you're OK with inconsistent permissions on Windows system drives, use **ntfs3**
* Otherwise you're probably better of with the default and more established **ntfs-3g**

## Limitations
- Only one drive can be mounted at a time (this might be improved in the future)
- Only Apple Silicon Macs are supported (libkrun limitation)

## Troubleshooting
- If having issues with `init` (cannot download from alpine for example), having a VPN enabled is a known issue so consider temporarily disabling your VPN to ensure `init` succeeds
- Check your mount flags (e.g. the `subvol` flag from demo is specific to btrfs, make sure you don't use it with other filesystems)
- Check file owner and permissions with `ls -l` and adjust accordingly. Typically, your macOS user won't have write access to your drive out of the box so you need to write files as root or first prepare a target directory writable by everyone (`chmod 777`).
- If you get `fcopyfile failed: Operation not permitted`, it can actually mean the file you're trying to copy has the quarantine attribute set (can be removed with `xattr -d com.apple.quarantine <filename>`)
- Accessing disks might require Full Disk Access permission (although you should get pop-ups that let you allow access case-by-case)

## Build from source

```
# build dependencies
brew install go rustup   # Skip if not relevant. You need Go and Rust toolchains but not necessarily installed via homebrew
brew install pkgconf filosottile/musl-cross/musl-cross # These are for cross-compiling the Linux helper running in the VM

# if you just installed rustup from homebrew
rustup default stable
export PATH="$PATH:/opt/homebrew/opt/rustup/bin"

# runtime dependencies
brew install util-linux slp/krun/libkrun # Libblkid library and the hypervisor - you should already have these if you installed anylinuxfs from homebrew

# building anylinuxfs
git clone https://github.com/nohajc/anylinuxfs.git
cd anylinuxfs
rustup target add aarch64-unknown-linux-musl
./download-dependencies.sh
./build-app.sh             # debug build
./build-app.sh --release   # release build

# compiled executable will be available under ./bin
bin/anylinuxfs list
```

## Acknowledgements
This project was made possible by
- [libkrun](https://github.com/containers/libkrun) the microVM hypervisor for Linux anc Mac
- [libkrunfw](https://github.com/containers/libkrunfw) - Linux kernel bundled for libkrun as a dynamic library
- [gvproxy](https://github.com/containers/gvisor-tap-vsock) - user space networking for virtual machines (also used by podman)
- [docker-nfs-server](https://github.com/ehough/docker-nfs-server) - launcher for NFS server in a container

Thank you all for your great work!
