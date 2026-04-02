# Examples

**List available drives with compatible filesystems**
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

**Mount partition read/write**
```
sudo anylinuxfs /dev/disk0s6
```

**Mount partition read-only**
```
sudo anylinuxfs /dev/disk0s6 -o ro
```

**Recover (fsck) a filesystem**

Replace e2fsprogs with relevant fsck package for your filesystem

```
anylinuxfs apk add e2fsprogs
anylinuxfs shell /dev/rdisk6s2
e2fsck /dev/vda
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

May reveal encrypted LVM volume groups or additional filesystem information
```
sudo anylinuxfs list -d all
```
[See LUKS/LVM details](luks-lvm.md)

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
