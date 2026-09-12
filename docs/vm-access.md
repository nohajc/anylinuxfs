# Accessing a running VM

When anylinuxfs mounts a filesystem, it keeps a VM running in the background to perform the Linux-side mount and expose it to macOS. You can open a shell in that VM or execute a command there with `anylinuxfs vm`.

This is useful for inspecting a mounted filesystem with Linux tools, performing one-off maintenance, or scripting operations which need to run in the same environment as the mount.

## Open an interactive shell

If exactly one anylinuxfs VM is running, open a shell in it with:
```
anylinuxfs vm attach
```

If more than one VM is running, specify the disk identifier or mount point:
```
anylinuxfs vm attach /Volumes/MyDrive
```

## Execute a command

Use `vm exec` to run a command without opening a shell:
```
anylinuxfs vm exec -- uname -a
```

As with `vm attach`, specify the disk identifier or mount point when more than one VM is running:
```
anylinuxfs vm exec /Volumes/MyDrive -- sh -c 'find / -maxdepth 1'
```

The `--` separates the anylinuxfs arguments from the program and its arguments. Command input and output are connected to your terminal, so `vm exec` also works with commands that prompt for input. The exit status of `anylinuxfs vm exec` is the exit status of the command in the VM.

## `vm` and `shell`

`anylinuxfs vm` connects to an existing VM which is already mounting a filesystem. In contrast, `anylinuxfs shell` starts a fresh VM configured from its mount options, without mounting and exporting the filesystem to macOS. Use `shell` for debugging or tasks which need direct access to a disk, such as filesystem repair; use `vm` to work with a running mount.
