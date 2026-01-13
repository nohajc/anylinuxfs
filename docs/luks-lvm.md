For any LUKS drives with LVM partitions, you cannot mount them directly as said in previous commands like `sudo anylinuxfs mount (example here)`, rather you might need to find the LVM volume group name, logical volume name as well and use the lvm: identifier in order to tell anylinuxfs that you are trying to mount a lvm drive. 

**Example for finding volume group** 
```
sudo anylinuxfs list -d all
```
Because of the `-d, --decrypt` flag, anylinuxfs will also prompt for any LUKS password here.
After that, you should see a layout of this:
```
lvm:<volume group name> (volume group)
#: TYPE NAME
0: LVM2_scheme SIZE                   IDENTIFIER 
               (Size of your drive).  <name of volume group>
                Physical Store disk7s3 
1:  <file system type> <size of lvm>   <name of volume group>:diskidentifier:<name of volume>
```
After this command, anylinuxfs will scan for any identified volume groups with the lvm: identifier. 

Then, you can properly mount the volume group like the example below and use before flags as necessary, like setting it to read only. 
> [!NOTE]
> This is just an example layout, things may look different as your disk sizes and disk identifiers vary on macOS. disk7s3 is just a placeholder to show you what it might look like. 

**Example for mounting a volume group** 
```
sudo anylinuxfs mount lvm:<vg-name>:<disk partition name>:<lv-name>
```
These are some basic examples, but anylinuxfs then after using this command will prompt you for your LUKS password to decrypt and you should be all set. 
