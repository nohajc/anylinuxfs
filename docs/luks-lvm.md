For any LUKS drives with LVM partitions, you cannot mount them directly using a `/dev/diskXsY` path, instead you will need to find the LVM volume group name, logical volume name and use the `lvm:` prefix in order to tell anylinuxfs what exactly to mount. 

**Finding volume groups** 
```
sudo anylinuxfs list -d all
```
Because of the `-d, --decrypt` flag, anylinuxfs will prompt for any LUKS passphrase needed.

Then it will scan for any LUKS-encrypted volume groups and their respective logical volumes. You should see output like this:
```
lvm:<volume group name> (volume group)
#: TYPE NAME
0: LVM2_scheme SIZE                   IDENTIFIER 
               (Size of your drive).  <name of volume group>
                Physical Store diskXsY 
1:  <file system type> <size of lvm>   <name of volume group>:diskidentifier:<name of volume>
```

Any of the `lvm:` identifiers shown can be used with the mount command. You can also pass flags as usual (e.g. mounting read-only, etc.). 

**Mounting a logical volume** 
```
sudo anylinuxfs mount lvm:<vg-name>:<disk partition path>:<lv-name>
```

Running this will prompt for your LUKS passphrase once again and you should be all set. 

> [!NOTE]
> In case your volume group spans multiple drives, you must specify all the respective `/dev/...` identifiers
>
> (e.g. `lvm:vg1:/dev/disk3s1:/dev/disk4s1:lv1`)
