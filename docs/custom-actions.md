# Custom actions

With custom actions, you're able to define sets of scripts which will run inside the virtual machine at specific points.
Currently supported actions: `before_mount`, `after_mount`, `before_unmount` (typically to do cleanup).
You can also override the path inside the virtual machine which gets shared with macOS via NFS. This is useful for mounting nested filesystems (from disk images, etc.).

The path override even makes it possible to run a mount action without any device identifier. We call that a diskless custom action - let's say your script connects to a remote server and mounts a disk image from there for example.

Your custom actions can also depend on additional packages not included in the base Linux installation by default. `anylinuxfs` exposes the Alpine package manager for that purpose. That means it can maintain a list of extra packages installed by the user and reinstall them again when you reinit your Linux image (or when reinit is forced by `anylinuxfs` upgrade).

## Pre-defined actions

There are custom actions that come pre-installed with `anylinuxfs`. You can check the `/opt/homebrew/etc/anylinuxfs.toml` config file. For any other actions that you define, use `~/.anylinuxfs/config.toml` instead.

## List available actions

To quickly check which actions are available, run `anylinuxfs actions` which will give you their names and descriptions. Any modifications are done in config files directly.

## Examples

### Mount borg backup located on a Linux drive

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
