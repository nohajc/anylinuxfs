# anylinuxfs
An easy way to mount ext4, btrfs or in fact any linux-supported filesystem on a Mac.
With full write support, based on the libkrun microVM hypervisor and NFS. Doesn't require installing any kernel extensions or lowering system security.

<a href='https://ko-fi.com/Q5Q41EHAGK' target='_blank'><img height='36' style='border:0px;height:36px;' src='https://storage.ko-fi.com/cdn/kofi1.png?v=6' border='0' alt='Buy Me a Coffee at ko-fi.com' /></a>

> [!IMPORTANT]
> If anylinuxfs is suddenly freezing, you are most probably affected by a new bug in the libkrun library.
> Please, upgrade to the latest version and read the [release notes](https://github.com/nohajc/anylinuxfs/releases/tag/v0.8.7).

## Features
- mounts any filesystem supported by Linux (**ext4**, **btrfs**, **xfs**, ... but also **NTFS** and **exFAT**)
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
- define your own [custom actions](#custom-actions) (e.g. mount **borg backup** located on a Linux drive)

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

You pick a drive, mount it with `anylinuxfs` and it appears as a NFS share on localhost. This spins up a microVM in the background which uses the real linux drivers, so you can access anything from `ext*` to `btrfs`. Any mount options on the command-line will be forwarded to the linux mount command, so you can mount read-only, read-write, pick btrfs subvolumes, etc. Then you simply eject the drive in Finder or use `anylinuxfs unmount` in terminal and the virtual machine will be turned off.

This all sounds like a lot of work but it's actually very fast. Not like a traditional virtual machine which takes a while to boot.
This one is just a stripped down version of Linux, there's not even a UEFI firmware. Practically, it takes only a couple of seconds before the drive is mounted and ready to use.

## Basic usage

Most often, you will probably use the following commands:
* `anylinuxfs mount` - mount a filesystem; this is the default command, so the `mount` keyword can be omitted
* `anylinuxfs unmount` - safe unmount, useful in case of multiple mounts (typically ZFS datasets) which need to be ejected in a particular order
* `anylinuxfs list` - show available Linux filesystems (alternatively, `anylinuxfs list -m` shows Microsoft filesystems)
* `anylinuxfs status` - show what is currently mounted
* `anylinuxfs log` - show details about the current (or last) run, useful for troubleshooting

### Mounting filesystems

From `anylinuxfs mount --help`:
```
Usage: anylinuxfs [mount] [OPTIONS] <DISK_IDENT> [MOUNT_POINT]

Arguments:
  <DISK_IDENT>   File path(s), LVM identifier or RAID identifier, e.g.:
                 /dev/diskXsY[:/dev/diskYsZ:...]
                 lvm:<vg-name>:diskXsY[:diskYsZ:...]:<lv-name>
                 raid:diskXsY[:diskYsZ:...]
                 (see `list` command output for available volumes)
  [MOUNT_POINT]  Custom mount path to override the default under /Volumes
```

* The only required parameter is the disk identifier.
* It must always refer to one or more partitions or logical volumes (not whole disks).
* Basic syntax of an identifier is `/dev/diskXsY` - based on how `anylinuxfs list` or `diskutil list` identifies your drives.
* If your filesystem is on a logical volume, you will usually need a special prefixed identifier starting with `lvm` or `raid` (for mdadm Linux RAID).
  These can be deduced from `anylinuxfs list` output where any logical volumes will be shown as synthesized disks (similar to how `diskutil` does it for APFS containers)
* In case of btrfs filesystems spanning multiple disks (like RAID1 or JBOD), these will not be grouped in the `anylinuxfs list` output.
* In order to mount a filesystem like this, you use the `/dev/diskXsY:/dev/diskYsZ` syntax. Basically, you must specify all partitions that need to be attached to our microVM so that they can be scanned for any multi-disk btrfs filesystems.

## Examples

**List available drives with Linux filesystems**
```
sudo anylinuxfs list
```

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

**List available drives with Microsoft filesystems (NTFS, exFAT, FAT32)**
```
sudo anylinuxfs list -m
```

**Mount partition read/write**
```
sudo anylinuxfs /dev/disk0s6
```

**Mount partition read-only**
```
sudo anylinuxfs /dev/disk0s6 -o ro
```

**Mount logical volume from group vg1 backed by disk7s1**
```
sudo anylinuxfs lvm:vg1:disk7s1:lvol0
```

**Mount logical volume from group vg1 backed by disk7s1 and disk8s1**
```
sudo anylinuxfs lvm:vg1:disk7s1:disk8s1:lvol2
```

**Mount RAID volume backed by disk10s1 and disk11s1**
```
sudo anylinuxfs raid:disk10s1:disk11s1
```

**List available drives and decrypt LUKS or BitLocker metadata of disk9**
```
sudo anylinuxfs list -d /dev/disk9
```

Output will show the encrypted partition filesystem and label
```
...
/dev/disk9 (external, physical):
   #:                       TYPE NAME                    SIZE       IDENTIFIER
   0:          crypto_LUKS: ext4 enc-ext4               *8.1 GB     disk9
...
```

**List available drives and decrypt all LUKS or BitLocker metadata**
[See here for more info](docs/lvm-luks.md)

**Mount LUKS-encrypted or BitLocker-encrypted partition**
```
# anylinuxfs will show an interactive passphrase prompt
sudo anylinuxfs /dev/disk9

# or it can take the passphrase from environment
export ALFS_PASSPHRASE="my_strong_password"
sudo -E anylinuxfs /dev/disk9
```

> [!NOTE]
> If you have more disks with different passphrases you can define variables named `ALFS_PASSPHRASE1`, `ALFS_PASSPHRASE2`, `ALFS_PASSPHRASE3`, etc.

**Mount partition and share it via NFS to other devices in any subnet**
```
sudo anylinuxfs /dev/disk0s6 -b 0.0.0.0
```

**Mount partition and share it via NFS to devices within your subnet (more secure)**
```
# by server, we mean the device which is sharing the mounted filesystem
sudo anylinuxfs /dev/disk0s6 -b <YOUR_SERVER_IP>
```

**Show current mount status**
```
anylinuxfs status
```

**Try to stop anylinuxfs in case umount or eject didn't completely terminate the VM**
```
anylinuxfs stop
```

## Custom actions

With custom actions, you're able to define sets of scripts which will run inside the virtual machine at specific points.
Currently supported actions: `before_mount`, `after_mount`, `before_unmount` (typically to do cleanup).
You can also override the path inside the virtual machine which gets shared with macOS via NFS. This is useful for mounting nested filesystems (from disk images, etc.).

Your custom actions can also depend on additional packages not included in the base Linux installation by default. `anylinuxfs` exposes the Alpine package manager for that purpose. That means it can maintain a list of extra packages installed by the user and reinstall them again when you reinit your Linux image (or when reinit is forced by `anylinuxfs` upgrade).

### Pre-defined actions

There are custom actions that come pre-installed with `anylinuxfs`. You can check the `/opt/homebrew/etc/anylinuxfs.toml` config file. For any other actions that you define, use `~/.anylinuxfs/config.toml` instead.

### List available actions

To quickly check which actions are available, run `anylinuxfs actions` which will give you their names and descriptions. Any modifications are done in config files directly.

### Examples

#### Mount borg backup located on a Linux drive

For this, we will first need to install additional alpine packages:
```
anylinuxfs apk add borgbackup fuse py3-llfuse
```

Then we define a custom action by editing (or creating) `~/.anylinuxfs/config.toml`:
```
[custom_actions.borg]
after_mount = "mkdir -p /mnt/borg && borg mount $ALFS_VM_MOUNT_POINT/$BORG_REPO /mnt/borg"
before_unmount = "borg umount /mnt/borg && rmdir /mnt/borg"
override_nfs_export = "/mnt/borg"
```

You can refer to environment variables from custom actions. Those starting with `$ALFS_` are set by anylinuxfs. Any other variables must be set by the user.
If you want your custom action to use any other environment variable which is not explicitly used in the script (e.g. borg might use `$BORG_PASSPHRASE`), you can list them like this:
```
[custom_actions.borg]
after_mount = "..."
before_unmount = "..."
capture_environment = ["BORG_PASSPHRASE"]
override_nfs_export = "/mnt/borg"
```

To invoke your action when mounting a drive, use the `-a` flag (and make sure sudo preserves your environment):
```
export BORG_REPO=<path to borg repo relative to mount point>
sudo -E anylinuxfs mount /dev/disk4s2 -a borg
```

You will be asked for your passphrase (if you haven't set `capture_environment` and exported `BORG_PASSPHRASE`) and your borg backup will be mounted instead of the whole Linux drive.

## Notes

### VM initialization
- When you first run `anylinuxfs` to mount a drive, it will download the alpine Linux image from Docker hub and unpack it to your user profile (`~/.anylinuxfs/alpine`).
Then it will spin up a VM so it can install dependencies and do the initial environment setup. After that, the Linux root filesystem will be reused for every mount operation.
You can also run `anylinuxfs init` to download a fresh copy of `alpine:latest` and reinitialize the environment at any time.

### Custom CA certificates
- If you need to add custom CA certificates for the alpine VM to download packages, you can do so by adding them to a file in your user profile (`~/.anylinuxfs/ca-certificates.crt`). The CA certificates must be in newline-separated PEM blocks. These will be appended to the alpine image defaults during the first run of `anylinuxfs`, or when calling `anylinuxfs init`.

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

**To summarize**
* There are stories online about data corruption (or system freezes) caused by the **ntfs3** driver.
* They might or might not be caused by improper use of `ntfsfix`.
* **ntfs3** is included in the mainline Linux kernel so it is considered stable. It was contributed by Paragon Software in [2021](https://www.paragon-software.com/paragon-software-announces-the-inclusion-of-its-ntfs3-driver-into-linux-kernel-5-15/).
* If you trust it, want the best performance and you're OK with inconsistent permissions on Windows system drives, use **ntfs3**
* Otherwise you're probably better of with the default and more established **ntfs-3g**

## Limitations
- Only one drive can be mounted at a time (this might be improved in the future)
- Only Apple Silicon Macs are supported (libkrun limitation)

## Troubleshooting
- Make sure nothing is running on ports 2049, 32765 and 32767. If there's another NFS server already running, `anylinuxfs` will not work.
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
