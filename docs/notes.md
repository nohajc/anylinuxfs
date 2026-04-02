# Notes

## VM initialization
- When you first run `anylinuxfs` to mount a drive, it will download the alpine Linux image from Docker hub and unpack it to your user profile (`~/.anylinuxfs/alpine`).
Then it will spin up a VM so it can install dependencies and do the initial environment setup. After that, the Linux root filesystem will be reused for every mount operation.
You can also run `anylinuxfs init` to download a fresh copy of `alpine:latest` and reinitialize the environment at any time.

## Custom CA certificates
- If you need to add custom CA certificates for the alpine VM to download packages, you can do so by adding them to a file in your user profile (`~/.anylinuxfs/ca-certificates.crt`). The CA certificates must be in newline-separated PEM blocks. These will be appended to the alpine image defaults during the first run of `anylinuxfs`, or when calling `anylinuxfs init`.

## Permissions
- It is needed to run mount commands with `sudo` otherwise we're not allowed direct access to `/dev/disk*` files. However, the virtual machine itself will in fact run under the regular user who invoked `sudo` in the first place (i.e. all unnecessary permissions are dropped after the disk is opened)

## Memory requirements for LUKS
- When you mount a LUKS-encrypted drive, the microVM requires at least 2.5 GiB RAM for cryptsetup to work properly. If your VM is configured with a lower amount of memory, you'll get a warning about it and the RAM configuration will be adjusted automatically. If you don't want to see the warning, set your default RAM to match this requirement (`anylinuxfs config -r <size-in-MiB>`)
- Configured amount of RAM is the maximum that can be allocated. The actual amount of memory consumed by the VM can be lower.

## NTFS
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
