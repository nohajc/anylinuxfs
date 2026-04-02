# Troubleshooting

## Port conflicts
- Make sure nothing is running on ports 2049, 32765 and 32767. If there's another NFS server already running, `anylinuxfs` will not work.

## Disk identifiers
- You should always mount individual partitions (e.g. `/dev/disk4s1`) not whole disks (`/dev/disk4`) unless they really don't have any partition table (which might be the case with LVM/LUKS containers sometimes).
- Check your mount flags (e.g. the `subvol` flag from demo is specific to btrfs, make sure you don't use it with other filesystems)

## File permissions
- Check file owner and permissions with `ls -l` and adjust accordingly. Typically, your macOS user won't have write access to your drive out of the box so you need to write files as root or first prepare a target directory writable by everyone (`chmod 777`).
- To bypass Unix file permissions and make files appear to be owned by the current macOS user, use the `--ignore-permissions` flag (e.g. `sudo anylinuxfs /dev/disk0s6 --ignore-permissions`). This squashes all UIDs/GIDs on the NFS export and sets the `noowners` NFS mount option. Equivalent to manually setting `--nfs-export-opts rw,no_subtree_check,all_squash,anonuid=0,anongid=0,insecure -n noowners`.
- If your drive appears mounted but you cannot browse any files (or the volume folder appears empty), it might also be a permission issue. When you run `ls -ld /Volumes/<your_drive>`, you will see something like `drwx------`. This can be fixed by running `sudo chmod go+rx /Volumes/<your_drive>`. Beware that this will effectively allow any user to browse your files though (at least in the root directory – other sensitive files are often protected individually). If this is not what you want, just use terminal commands with `sudo` for any file operations.

## Quarantine attribute
- If you get `fcopyfile failed: Operation not permitted`, it can actually mean the file you're trying to copy has the quarantine attribute set (can be removed with `xattr -d com.apple.quarantine <filename>`)

## Full Disk Access
- Accessing disks might require Full Disk Access permission (although you should get pop-ups that let you allow access case-by-case)
