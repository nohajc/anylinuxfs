For any LUKS drives with LVM partitions, you cannot mount them directly as said in previous commands like `sudo anylinuxfs mount (example here)`, rather you might need to find the LVM volume group name, logical volume name as well and use the lvm: identifier in order to tell anylinuxfs that you are trying to mount a lvm drive. 

**Example for finding volume group** 
```
sudo anylinuxfs list -d all
```

After this command, anylinuxfs will scan for any identified volume groups with the lvm: identifier. 

Then, you can properly mount the volume group like the example below and use before flags as necessary, like setting it to read only, and write only. 

**Example for mounting a volume group** 
```
sudo anylinuxfs mount lvm:<vg-name>:<disk partition name>:<lv-name>
```

These are some basic examples, but anylinuxfs then after using this command will prompt you for your LUKS password to decrypt and you should be all set. 
