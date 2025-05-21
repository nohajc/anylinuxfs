use anyhow::{Context, anyhow};
use common_utils::host_println;
use derive_more::AddAssign;
use indexmap::IndexMap;
use libc::{SIGINT, SIGTERM};
use nix::sys::signal::Signal;
use objc2_core_foundation::{
    CFDictionary, CFRetained, CFRunLoop, CFString, CFURL, kCFRunLoopDefaultMode,
};
use objc2_disk_arbitration::{
    DADisk, DARegisterDiskAppearedCallback, DARegisterDiskDisappearedCallback, DASession,
    DAUnregisterCallback,
};
use regex::Regex;
use serde::Deserialize;
use signal_hook::iterator::Signals;
use std::{
    ffi::{CString, c_void},
    fmt::Display,
    hash::{Hash, Hasher},
    iter,
    marker::PhantomData,
    ops::Deref,
    path::Path,
    process::Command,
    ptr::{NonNull, null, null_mut},
    str::FromStr,
    thread,
};
use url::Url;

use crate::{devinfo::DevInfo, fsutil};

pub struct Entry(String, String, String, Vec<String>);

impl Entry {
    pub fn new(disk: impl Into<String>) -> Self {
        Entry(
            disk.into(),
            String::default(),
            String::default(),
            Vec::new(),
        )
    }

    pub fn disk(&self) -> &str {
        self.0.as_str()
    }

    pub fn disk_mut(&mut self) -> &mut String {
        &mut self.0
    }

    pub fn header(&self) -> &str {
        self.1.as_str()
    }

    pub fn header_mut(&mut self) -> &mut String {
        &mut self.1
    }

    pub fn scheme(&self) -> &str {
        self.2.as_str()
    }

    pub fn scheme_mut(&mut self) -> &mut String {
        &mut self.2
    }

    pub fn partitions(&self) -> &[String] {
        &self.3
    }

    pub fn partitions_mut(&mut self) -> &mut Vec<String> {
        &mut self.3
    }
}

pub struct List(Vec<Entry>);

impl Display for List {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for entry in &self.0 {
            if entry.partitions().is_empty() {
                continue;
            }
            writeln!(f, "{}", entry.disk())?;
            if !entry.header().is_empty() {
                writeln!(f, "{}", entry.header())?;
            }
            if !entry.scheme().is_empty() {
                writeln!(f, "{}", entry.scheme())?;
            }
            for partition in entry.partitions() {
                writeln!(f, "{}", partition)?;
            }
            writeln!(f, "")?;
        }
        Ok(())
    }
}

fn trunc_with_ellipsis(s: &str, max_len: usize) -> String {
    if s.len() > max_len {
        format!("{}...", &s[..max_len - 3])
    } else {
        s.to_string()
    }
}

fn diskutil_list_from_plist() -> anyhow::Result<Plist> {
    let output = Command::new("diskutil")
        .arg("list")
        .arg("-plist")
        .output()
        .expect("Failed to execute diskutil");

    if !output.status.success() {
        return Err(anyhow!("diskutil command failed"));
    }

    let plist: Plist = plist::from_bytes(&output.stdout).context("Failed to parse plist")?;
    Ok(plist)
}

fn disks_without_partition_table(plist: &Plist) -> Vec<String> {
    let mut disks = Vec::new();
    for disk in &plist.all_disks_and_partitions {
        if disk.partitions.is_none() && disk.content.as_deref() == Some("") {
            disks.push(disk.device_identifier.clone());
        }
    }
    disks
}

// normally, we match any filesystem with the following partition type
const LINUX_PART_TYPES: [&str; 4] = ["Linux Filesystem", "Linux LVM", "Linux_LVM", "Linux"];
// static fs list only used for matching drives without any partition table
const LINUX_FS_TYPES: [&str; 10] = [
    "btrfs",
    "erofs",
    "ext2",
    "ext3",
    "ext4",
    "squashfs",
    "xfs",
    "zfs",
    "crypto_LUKS",
    "LVM2_member",
];

fn partitions_with_linux_fs(plist: &Plist) -> Vec<String> {
    let mut partitions = Vec::new();
    for disk in &plist.all_disks_and_partitions {
        if let Some(partitions_list) = &disk.partitions {
            for partition in partitions_list {
                if LINUX_PART_TYPES
                    .into_iter()
                    .any(|fs_type| partition.content.as_deref() == Some(fs_type))
                {
                    partitions.push(partition.device_identifier.clone());
                }
            }
        }
    }
    partitions
}

fn augment_line(line: &str, part_type: &str, dev_info: Option<&DevInfo>, fs_type: &str) -> String {
    let label = trunc_with_ellipsis(
        dev_info
            .map(|di| di.label())
            .flatten()
            .unwrap_or("                       "),
        23,
    );
    line.replace(
        &format!("{:>27} {:<23}", part_type, ""),
        &format!("{:>27} {:<23}", fs_type, label),
    )
}

fn lv_size_split_val_and_units(size: &str) -> (&str, String) {
    let size_last_char = size.chars().last().unwrap_or('0');
    let (size_val, unit_prefix) = if size_last_char.is_digit(10) {
        (size, "".to_string())
    } else {
        (size.strip_suffix(|_| true).unwrap(), size_last_char.into())
    };

    (size_val, unit_prefix)
}

fn format_lv_size(size: &str) -> String {
    let (size_val, unit_prefix) = lv_size_split_val_and_units(size);

    let mut size_val = size_val.parse::<f64>().unwrap_or(0.0);
    // lsblk actually shows sizes in KiB, MiB, GiB, TiB, PiB, EiB
    // so we need to convert them to KB, MB, GB, TB, PB, EB
    size_val = match unit_prefix.as_str() {
        "K" => size_val as f64 * 1.024,
        "M" => size_val as f64 * 1.048576,
        "G" => size_val as f64 * 1.073741824,
        "T" => size_val as f64 * 1.099511627776,
        "P" => size_val as f64 * 1.125899906842624,
        "E" => size_val as f64 * 1.152921504606847,
        _ => size_val as f64,
    };

    format!("{:.1} {}B", size_val, unit_prefix)
}

#[derive(AddAssign, Debug)]
struct LvSize(u64);

impl Display for LvSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut size_integer = self.0 * 10;
        let prefixes = ["", "K", "M", "G", "T", "P", "E"];
        let mut unit_prefix = "";

        let mut size_rem = 0;
        for &p in &prefixes {
            if size_integer < 1000 {
                unit_prefix = p;
                break;
            }
            size_rem = size_integer % 1000;
            size_integer /= 1000;
        }
        let size = size_integer as f64 / 10.0 + size_rem as f64 / 10000.0;

        format!("{:.1} {}B", size, unit_prefix).fmt(f)
    }
}

fn parse_lv_size(size: &str) -> anyhow::Result<LvSize> {
    let (size_val, unit_prefix) = lv_size_split_val_and_units(size);

    // lsblk actually shows sizes in KiB, MiB, GiB, TiB, PiB, EiB
    // so we need to convert them to KB, MB, GB, TB, PB, EB
    let size_integer = (size_val.parse::<f64>().unwrap_or(0.0) * 10.0) as u64;
    let size_bytes = match unit_prefix.as_str() {
        "K" => size_integer * 1024,
        "M" => size_integer * 1024 * 1024,
        "G" => size_integer * 1024 * 1024 * 1024,
        "T" => size_integer * 1024 * 1024 * 1024 * 1024,
        "P" => size_integer * 1024 * 1024 * 1024 * 1024 * 1024,
        "E" => size_integer * 1024 * 1024 * 1024 * 1024 * 1024 * 1024,
        _ => size_integer,
    } / 10;

    // println!("DEBUG: size={size}, size_bytes={size_bytes}, unit_prefix={unit_prefix}");

    Ok(LvSize(size_bytes))
}

#[derive(Debug)]
struct LvIdent {
    vg_name: String,
    lv_name: String,
}

impl FromStr for LvIdent {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut chars = s.chars().peekable();
        let mut vg_name: String = "".into();

        while chars.peek().is_some() {
            vg_name += &iter::from_fn(|| chars.by_ref().next_if(|&c| c != '-')).collect::<String>();
            let dash_count = &iter::from_fn(|| chars.by_ref().next_if(|&c| c == '-')).count();
            vg_name += &"-".repeat(dash_count / 2);
            if dash_count % 2 == 1 {
                break;
            }
        }
        let lv_name = chars.collect::<String>().replace("--", "-");

        if vg_name.is_empty() || lv_name.is_empty() {
            return Err(anyhow!("Invalid LV identifier: {}", s));
        }
        Ok(LvIdent { vg_name, lv_name })
    }
}

pub fn list_linux_partitions(
    config: crate::Config,
    enc_partition: Option<&str>,
) -> anyhow::Result<List> {
    let numbered_pattern = Regex::new(r"^\s+\d+:").unwrap();
    let part_type_pattern = Regex::new(&format!(r"({})", LINUX_PART_TYPES.join("|"))).unwrap();
    let mut disk_entries = Vec::new();

    let plist_out = diskutil_list_from_plist()?;
    // println!("plist_out: {:#?}", plist_out);
    let linux_partitions = partitions_with_linux_fs(&plist_out);
    // println!("linux_partitions: {:?}", linux_partitions);
    let disks_without_part_table = disks_without_partition_table(&plist_out);
    // println!("disks_without_part_table: {:?}", disks_without_part_table);
    let mut lvm_luks_dev_infos = Vec::new();
    let mut lvm_luks_dev_idents = Vec::new();

    let output = Command::new("diskutil")
        .arg("list")
        .output()
        .expect("Failed to execute diskutil");

    if !output.status.success() {
        return Err(anyhow!("diskutil command failed"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut current_entry = None;

    for line in stdout.lines() {
        if line.starts_with("/dev/disk") {
            disk_entries.push(Entry::new(line));
            let last_idx = disk_entries.len() - 1;
            current_entry = disk_entries.get_mut(last_idx)
        } else if line.trim_start().starts_with("#:") {
            current_entry.as_mut().map(|entry| {
                entry.header_mut().push_str(line);
            });
        } else if numbered_pattern.is_match(line) {
            let Some(dev_ident) = line.split_whitespace().last() else {
                continue;
            };
            if let Some(part_type) = part_type_pattern.find(line).map(|m| m.as_str()) {
                // check the device identifier against partition list we parsed from plist
                // (otherwise regex matching alone might give false positives)
                if !linux_partitions.iter().any(|p| p == dev_ident) {
                    continue;
                }
                let disk_path = format!("/dev/{dev_ident}");
                let dev_info = DevInfo::pv(&disk_path).ok();

                let line = match dev_info {
                    Some(dev_info) => {
                        let fs_type = dev_info.fs_type().unwrap_or(part_type);
                        if fs_type == "LVM2_member"
                            || (enc_partition.is_some() && fs_type == "crypto_LUKS")
                        {
                            lvm_luks_dev_infos.push(dev_info.clone());
                            lvm_luks_dev_idents.push(dev_ident.to_owned());
                        }
                        augment_line(line, part_type, Some(&dev_info), fs_type)
                    }
                    None => line.to_owned(),
                };
                current_entry.as_mut().map(|entry| {
                    entry.partitions_mut().push(line);
                });
            } else if line.trim_start().starts_with("0:") {
                if disks_without_part_table.iter().any(|d| d == dev_ident) {
                    // This is a disk without partition table, it might still contain a Linux filesystem
                    let dev_info = DevInfo::pv(&format!("/dev/{dev_ident}")).ok();

                    let fs_type = dev_info
                        .as_ref()
                        .map(|di| di.fs_type())
                        .flatten()
                        .unwrap_or("Unknown");
                    // if DevInfo is available, show linux fs types only
                    if fs_type != "Unknown" && !LINUX_FS_TYPES.into_iter().any(|t| t == fs_type) {
                        continue;
                    }
                    let line = augment_line(line, "", dev_info.as_ref(), fs_type);
                    current_entry.as_mut().map(|entry| {
                        entry.partitions_mut().push(line);
                    });
                } else {
                    current_entry.as_mut().map(|entry| {
                        entry.scheme_mut().push_str(line);
                    });
                }
            }
        }
    }

    if lvm_luks_dev_infos.len() > 0 {
        match get_lsblk_info(&config, &lvm_luks_dev_infos, enc_partition) {
            Ok(lsblk) => {
                // println!("lsblk: {:#?}", lsblk);
                if !lsblk.blockdevices.is_empty() {
                    let logical_volumes = create_volume_map(&lsblk, &lvm_luks_dev_idents);
                    // println!("logical_volumes: {:#?}", logical_volumes);

                    for (
                        vg_name,
                        VgEntry {
                            vg_size,
                            vg_dev_idents,
                            lvs,
                        },
                    ) in &logical_volumes
                    {
                        let mut lvm_entry = Entry::new("");
                        lvm_entry.header_mut().push_str(
                        "   #:                       TYPE NAME                    SIZE       IDENTIFIER"
                        );

                        for (j, (child, devs)) in lvs.iter().enumerate() {
                            let lv_ident = child.name.parse::<LvIdent>().unwrap();
                            let dev_ident = devs.join(":");
                            lvm_entry.partitions_mut().push(format!(
                                "{:>4}: {:>26} {:<23} {:<10} {}",
                                j + 1,
                                child.fstype.as_deref().unwrap_or(""),
                                child.label.as_deref().unwrap_or(""),
                                format_lv_size(&child.size),
                                format!(
                                    "{}:{}:{}",
                                    &lv_ident.vg_name, &dev_ident, &lv_ident.lv_name
                                ),
                            ));
                        }

                        if !lvm_entry.partitions().is_empty() {
                            *lvm_entry.disk_mut() = format!("lvm:{} (volume group):", &vg_name);
                            *lvm_entry.scheme_mut() = format!(
                                "   0:                LVM2_scheme                        +{:<10} {}",
                                vg_size, &vg_name
                            );

                            let mut label = "Physical Store";
                            for dev_ident in vg_dev_idents {
                                *lvm_entry.scheme_mut() +=
                                    &format!("\n{:<32} {} {}", "", label, dev_ident);
                                label = "              ";
                            }

                            disk_entries.push(lvm_entry);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to get lsblk info: {:#}", e);
            }
        }
    }

    Ok(List(disk_entries))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlkDevKind {
    Simple,
    LVM,
    LUKS,
}

impl BlkDevKind {
    fn from_fstype(fstype: Option<&str>) -> Self {
        match fstype {
            Some("LVM2_member") => BlkDevKind::LVM,
            Some("crypto_LUKS") => BlkDevKind::LUKS,
            _ => BlkDevKind::Simple,
        }
    }
}

fn create_volume_map(lsblk: &LsBlk, lvm_dev_idents: &[String]) -> IndexMap<String, VgEntry> {
    let mut logical_volumes: IndexMap<String /* vg_name */, VgEntry> = IndexMap::new();

    fn iterate_children(
        logical_volumes: &mut IndexMap<String, VgEntry>,
        dev_ident: &str,
        blkdev: &LsBlkDevice,
        kind: BlkDevKind,
        children: Option<&Vec<LsBlkDevice>>,
    ) {
        for (j, child) in children.into_iter().flatten().enumerate() {
            let child_kind = BlkDevKind::from_fstype(child.fstype.as_deref());

            if child_kind == BlkDevKind::Simple && kind == BlkDevKind::LVM {
                if let Ok(lv_ident) = child.name.parse::<LvIdent>() {
                    // println!("lv_ident: {:#?}", &lv_ident);
                    let VgEntry {
                        vg_size,
                        vg_dev_idents,
                        lvs,
                    } = logical_volumes
                        .entry(lv_ident.vg_name.to_string())
                        .or_insert_with(VgEntry::default);

                    if j == 0 {
                        *vg_size += parse_lv_size(&blkdev.size).unwrap_or(LvSize(0));

                        vg_dev_idents.push(dev_ident.into());
                    }

                    lvs.entry(child.clone())
                        .or_insert_with(Vec::new)
                        .push(dev_ident.into());
                }
            }

            iterate_children(
                logical_volumes,
                dev_ident,
                child,
                child_kind,
                child.children.as_ref(),
            );
        }
    }

    for (i, blkdev) in lsblk.blockdevices.iter().enumerate() {
        let dev_ident = &lvm_dev_idents[i];
        let kind = BlkDevKind::from_fstype(blkdev.fstype.as_deref());

        iterate_children(
            &mut logical_volumes,
            dev_ident,
            blkdev,
            kind,
            blkdev.children.as_ref(),
        )
    }

    logical_volumes
}

fn get_lsblk_info(
    config: &crate::Config,
    dev_info: &[DevInfo],
    enc_partition: Option<&str>,
) -> anyhow::Result<LsBlk> {
    let enc_part_idx = dev_info
        .iter()
        .position(|di| Some(di.disk()) == enc_partition);
    let decrypt_script = match enc_part_idx {
        Some(idx) => format!(
            "cryptsetup open /dev/vd{} luks; ",
            ('a'..='z')
                .nth(idx)
                .context("block device index out of range")?
        ),
        None => String::new(),
    };
    let script = format!(
        "{}/sbin/vgchange -ay >/dev/null; /bin/lsblk -O --json",
        decrypt_script
    );
    // println!("lsblk script: {}", &script);
    let lsblk_args = vec![
        CString::new("/bin/busybox").unwrap(),
        CString::new("sh").unwrap(),
        CString::new("-c").unwrap(),
        CString::new(script.as_str()).unwrap(),
    ];
    let prompt_fn = enc_partition.map(|partition| {
        |in_fd| {
            // prompt user for passphrase
            let passphrase = rpassword::prompt_password(format!(
                "Enter passphrase for {}: ",
                partition.to_owned()
            ))
            .context("Failed to read passphrase")?;

            unsafe { crate::write_to_pipe(in_fd, format!("{passphrase}\n").as_bytes()) }
                .context("Failed to write to pipe")?;

            Ok(())
        }
    });
    let lsblk_cmd = crate::run_vmcommand(config, dev_info, false, true, lsblk_args, prompt_fn)
        .context("Failed to run command in microVM")?;
    // let lsblk_output =
    //     String::from_utf8(lsblk_cmd.output).context("Failed to convert lsblk output to String")?;
    // println!("lsblk_status: {}", &lsblk_cmd.status);
    // println!("lsblk_output: {}", &lsblk_output);
    if lsblk_cmd.status != 0 {
        return Err(anyhow!("lsblk command failed"));
    }

    // println!("{}", String::from_utf8_lossy(&lsblk_cmd.stdout));
    eprintln!("{}", String::from_utf8_lossy(&lsblk_cmd.stderr));

    let lsblk = serde_json::from_slice(&lsblk_cmd.stdout)
        .context("failed to parse lsblk command output")?;

    Ok(lsblk)
}

unsafe fn cfdict_get_value<'a, T>(dict: &'a CFDictionary, key: &str) -> Option<&'a T> {
    let key = CFString::from_str(key);
    let key_ptr: *const CFString = key.deref();
    let mut value_ptr: *const c_void = null();
    let key_found = unsafe { dict.value_if_present(key_ptr as *const c_void, &mut value_ptr) };

    if !key_found {
        return None;
    }
    unsafe { (value_ptr as *const T).as_ref() }
}

struct DaDiskArgs<ContextType> {
    context: *mut c_void,
    descr: Option<CFRetained<CFDictionary>>,
    phantom: PhantomData<ContextType>,
}

impl<ContextType> DaDiskArgs<ContextType> {
    fn new(disk: NonNull<DADisk>, context: *mut c_void) -> Self {
        let descr = unsafe { DADisk::description(disk.as_ref()) };
        Self {
            context,
            descr,
            phantom: PhantomData,
        }
    }

    fn context(&self) -> &ContextType {
        unsafe { (self.context as *const ContextType).as_ref().unwrap() }
    }

    fn context_mut(&mut self) -> &mut ContextType {
        unsafe { (self.context as *mut ContextType).as_mut().unwrap() }
    }

    fn descr(&self) -> Option<&CFDictionary> {
        self.descr.as_ref().map(|d| d.deref())
    }

    fn volume_path(&self) -> Option<String> {
        let volume_path: Option<&CFURL> =
            unsafe { cfdict_get_value(self.descr()?, "DAVolumePath") };
        volume_path
            .map(|url| CFURL::string(url).to_string())
            .and_then(|url_str| Url::parse(&url_str).ok())
            .map(|url| url.path().to_string())
    }

    fn volume_kind(&self) -> Option<String> {
        let volume_kind: Option<&CFString> =
            unsafe { cfdict_get_value(self.descr()?, "DAVolumeKind") };
        volume_kind.map(|kind| kind.to_string())
    }
}

unsafe extern "C-unwind" fn disk_mount_event(disk: NonNull<DADisk>, context: *mut c_void) {
    let mut args = DaDiskArgs::<MountContext>::new(disk, context);

    if let (Some(volume_path), Some(volume_kind)) = (args.volume_path(), args.volume_kind()) {
        let expected_nfs_path = args.context().nfs_path;
        if volume_kind == "nfs" {
            if let Ok(dev_path) = fsutil::mounted_from(&volume_path) {
                if dev_path == expected_nfs_path {
                    args.context_mut().real_mount_point = Some(volume_path.clone());
                    CFRunLoop::stop(&CFRunLoop::main().unwrap());
                }
            }
        }
    }
}

unsafe extern "C-unwind" fn disk_unmount_event(disk: NonNull<DADisk>, context: *mut c_void) {
    let args = DaDiskArgs::<UnmountContext>::new(disk, context);

    if let (Some(volume_path), Some(volume_kind)) = (args.volume_path(), args.volume_kind()) {
        let expected_mount_point = args.context().mount_point;
        if volume_kind == "nfs" && volume_path == expected_mount_point {
            CFRunLoop::stop(&CFRunLoop::main().unwrap());
        }
    }
}

// unsafe extern "C-unwind" fn disk_unmount_approval(
//     disk: NonNull<DADisk>,
//     context: *mut c_void,
// ) -> *const DADissenter {
//     let args = DaDiskArgs::new(disk, context);
//     if let Some(descr) = args.descr() {
//         inspect_cf_dictionary_values(descr);
//     }
//     if let (Some(volume_path), Some(volume_kind)) = (args.volume_path(), args.volume_kind()) {
//         let expected_share_path = format!("/Volumes/{}/", args.share_name());
//         if volume_kind == "nfs" && volume_path == expected_share_path {
//             host_println!("Approve unmount of {}? [y/n]", &expected_share_path);
//             let mut input = String::new();
//             io::stdin().read_line(&mut input).unwrap();
//             if input.trim() == "y" {
//                 return null();
//             }
//         }
//     }
//     let msg = CFString::from_str("custom error message");
//     let result = unsafe { DADissenterCreate(None, kDAReturnBusy, Some(&msg)) };
//     msg.retain();
//     result.retain();
//     result.deref()
// }

// fn inspect_cf_dictionary_values(dict: &CFDictionary) {
//     let count = unsafe { CFDictionaryGetCount(dict) } as usize;
//     let mut keys: Vec<*const c_void> = vec![null(); count];
//     let mut values: Vec<*const c_void> = vec![null(); count];

//     unsafe { CFDictionaryGetKeysAndValues(dict, keys.as_mut_ptr(), values.as_mut_ptr()) };

//     for i in 0..count {
//         let value = values[i] as *const CFType;
//         let type_id = unsafe { CFGetTypeID(value.as_ref()) };
//         let type_name = CFCopyTypeIDDescription(type_id).unwrap();
//         let key_str = keys[i] as *const CFString;

//         host_println!(
//             "Key: {}, Type: {}",
//             unsafe { key_str.as_ref().unwrap() },
//             &type_name,
//         );
//     }
// }

struct MountContext<'a> {
    nfs_path: &'a Path,
    // what the OS assigned after resolving any potential conflicts
    real_mount_point: Option<String>,
}

impl<'a> MountContext<'a> {
    fn new(nfs_path: &'a Path) -> Self {
        Self {
            nfs_path,
            real_mount_point: None,
        }
    }
}

struct UnmountContext<'a> {
    mount_point: &'a str,
}

fn stop_run_loop_on_signal() -> anyhow::Result<()> {
    let mut signals = Signals::new(&[SIGINT, SIGTERM]).context("failed to register signals")?;
    _ = thread::spawn(move || {
        for signal in signals.forever() {
            match signal {
                SIGINT | SIGTERM => {
                    host_println!(
                        "Received signal {}",
                        Signal::try_from(signal)
                            .map(|s| s.to_string())
                            .unwrap_or("<unknown>".to_owned())
                    );
                    CFRunLoop::stop(&CFRunLoop::main().unwrap());
                    break;
                }
                _ => {}
            }
        }
    });

    Ok(())
}

pub struct MountPoint(String);

impl MountPoint {
    pub fn real(&self) -> &str {
        self.0.as_str()
    }

    // macOS disk events contain mount points with trailing slashes
    // however, we want to remove them for display purposes
    pub fn display(&self) -> &str {
        self.0.as_str().trim_end_matches('/')
    }
}

pub struct EventSession {
    session: CFRetained<DASession>,
}

impl EventSession {
    pub fn new() -> anyhow::Result<Self> {
        let session = unsafe { DASession::new(None).unwrap() };
        stop_run_loop_on_signal()?;
        Ok(Self { session })
    }

    // returns None when interrupted by SIGINT/SIGTERM
    pub fn wait_for_mount(&self, nfs_path: &Path) -> Option<MountPoint> {
        let mut mount_ctx = MountContext::new(nfs_path);
        let mount_ctx_ptr = &mut mount_ctx as *mut MountContext;
        unsafe {
            DARegisterDiskAppearedCallback(
                &self.session,
                None,
                Some(disk_mount_event),
                mount_ctx_ptr as *mut c_void,
            )
        };

        unsafe {
            DASession::schedule_with_run_loop(
                &self.session,
                &CFRunLoop::main().unwrap(),
                kCFRunLoopDefaultMode.unwrap(),
            )
        };

        CFRunLoop::run();

        let callback_ptr = disk_mount_event as *const c_void as *mut c_void;
        let callback_nonnull: NonNull<c_void> = NonNull::new(callback_ptr).unwrap();
        unsafe { DAUnregisterCallback(&self.session, callback_nonnull, null_mut()) };

        mount_ctx.real_mount_point.map(MountPoint)
    }

    pub fn wait_for_unmount(&self, mount_point: &str) {
        let mut unmount_ctx = UnmountContext { mount_point };
        let mount_ctx_ptr = &mut unmount_ctx as *mut UnmountContext;
        unsafe {
            DARegisterDiskDisappearedCallback(
                &self.session,
                None,
                Some(disk_unmount_event),
                mount_ctx_ptr as *mut c_void,
            )
        };

        // unsafe {
        //     DARegisterDiskEjectApprovalCallback(
        //         &session,
        //         None,
        //         Some(disk_unmount_approval),
        //         mount_ctx_ptr as *mut c_void,
        //     )
        // }

        unsafe {
            DASession::schedule_with_run_loop(
                &self.session,
                &CFRunLoop::main().unwrap(),
                kCFRunLoopDefaultMode.unwrap(),
            )
        };

        CFRunLoop::run();

        let callback_ptr = disk_unmount_event as *const c_void as *mut c_void;
        let callback_nonnull: NonNull<c_void> = NonNull::new(callback_ptr).unwrap();
        unsafe { DAUnregisterCallback(&self.session, callback_nonnull, null_mut()) };
    }
}

#[derive(Debug, Deserialize)]
struct Plist {
    #[serde(rename = "AllDisksAndPartitions")]
    all_disks_and_partitions: Vec<Disk>,
}

#[allow(unused)]
#[derive(Debug, Deserialize)]
struct Disk {
    #[serde(rename = "Content")]
    content: Option<String>,
    #[serde(rename = "DeviceIdentifier")]
    device_identifier: String,
    #[serde(rename = "OSInternal")]
    os_internal: Option<bool>,
    #[serde(rename = "Size")]
    size: Option<u64>,
    #[serde(rename = "Partitions")]
    partitions: Option<Vec<Partition>>,
    #[serde(rename = "APFSPhysicalStores")]
    apfs_physical_stores: Option<Vec<PhysicalStore>>,
    #[serde(rename = "APFSVolumes")]
    apfs_volumes: Option<Vec<ApfsVolume>>,
}

#[allow(unused)]
#[derive(Debug, Deserialize)]
struct Partition {
    #[serde(rename = "Content")]
    content: Option<String>,
    #[serde(rename = "DeviceIdentifier")]
    device_identifier: String,
    #[serde(rename = "DiskUUID")]
    disk_uuid: Option<String>,
    #[serde(rename = "Size")]
    size: Option<u64>,
    #[serde(rename = "VolumeName")]
    volume_name: Option<String>,
    #[serde(rename = "VolumeUUID")]
    volume_uuid: Option<String>,
}

#[allow(unused)]
#[derive(Debug, Deserialize)]
struct PhysicalStore {
    #[serde(rename = "DeviceIdentifier")]
    device_identifier: String,
}

#[allow(unused)]
#[derive(Debug, Deserialize)]
struct ApfsVolume {
    #[serde(rename = "CapacityInUse")]
    capacity_in_use: Option<u64>,
    #[serde(rename = "DeviceIdentifier")]
    device_identifier: String,
    #[serde(rename = "DiskUUID")]
    disk_uuid: Option<String>,
    #[serde(rename = "MountPoint")]
    mount_point: Option<String>,
    #[serde(rename = "MountedSnapshots")]
    mounted_snapshots: Option<Vec<Snapshot>>,
    #[serde(rename = "OSInternal")]
    os_internal: Option<bool>,
    #[serde(rename = "Size")]
    size: Option<u64>,
    #[serde(rename = "VolumeName")]
    volume_name: Option<String>,
    #[serde(rename = "VolumeUUID")]
    volume_uuid: Option<String>,
}

#[allow(unused)]
#[derive(Debug, Deserialize)]
struct Snapshot {
    #[serde(rename = "Sealed")]
    sealed: Option<String>,
    #[serde(rename = "SnapshotBSD")]
    snapshot_bsd: Option<String>,
    #[serde(rename = "SnapshotMountPoint")]
    snapshot_mount_point: Option<String>,
    #[serde(rename = "SnapshotName")]
    snapshot_name: Option<String>,
    #[serde(rename = "SnapshotUUID")]
    snapshot_uuid: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LsBlk {
    blockdevices: Vec<LsBlkDevice>,
}

#[derive(Debug)]
struct VgEntry {
    vg_size: LvSize,
    vg_dev_idents: Vec<String>,
    lvs: IndexMap<LsBlkDevice /* lv_uuid */, Vec<String /* dev_ident */>>,
}

impl Default for VgEntry {
    fn default() -> Self {
        Self {
            vg_size: LvSize(0),
            vg_dev_idents: Vec::new(),
            lvs: IndexMap::new(),
        }
    }
}

#[allow(unused)]
#[derive(Debug, Deserialize, Clone)]
struct LsBlkDevice {
    name: String,
    path: String,
    #[serde(default)]
    size: String,
    fstype: Option<String>,
    fsver: Option<String>,
    label: Option<String>,
    uuid: Option<String>,
    fsavail: Option<String>,
    #[serde(rename = "fsuse%")]
    fsuse_percent: Option<String>,
    mountpoints: Vec<Option<String>>,
    children: Option<Vec<LsBlkDevice>>,
}

impl PartialEq for LsBlkDevice {
    fn eq(&self, other: &Self) -> bool {
        self.uuid == other.uuid
    }
}

impl Eq for LsBlkDevice {}

impl Hash for LsBlkDevice {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.uuid.hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lv_ident_from_str() {
        let input = "vgname-lvname";
        let lv_ident = input.parse::<LvIdent>().unwrap();
        assert_eq!(lv_ident.vg_name, "vgname");
        assert_eq!(lv_ident.lv_name, "lvname");

        let input_with_dash = "vgname--withdash-lvname--withdash";
        let lv_ident_with_dash = input_with_dash.parse::<LvIdent>().unwrap();
        assert_eq!(lv_ident_with_dash.vg_name, "vgname-withdash");
        assert_eq!(lv_ident_with_dash.lv_name, "lvname-withdash");

        let input_with_trailing_dash = "vgname---lvname--";
        let lv_ident_with_trailing_dash = input_with_trailing_dash.parse::<LvIdent>().unwrap();
        assert_eq!(lv_ident_with_trailing_dash.vg_name, "vgname-");
        assert_eq!(lv_ident_with_trailing_dash.lv_name, "lvname-");

        let input_with_leading_dash = "---lvname";
        let lv_ident_with_leading_dash = input_with_leading_dash.parse::<LvIdent>().unwrap();
        assert_eq!(lv_ident_with_leading_dash.vg_name, "-");
        assert_eq!(lv_ident_with_leading_dash.lv_name, "lvname");

        let input_with_double_dash = "vg--long--name-lvname";
        let lv_ident_with_double_dash = input_with_double_dash.parse::<LvIdent>().unwrap();
        assert_eq!(lv_ident_with_double_dash.vg_name, "vg-long-name");
        assert_eq!(lv_ident_with_double_dash.lv_name, "lvname");

        let invalid_input = "invalidinput";
        assert!(invalid_input.parse::<LvIdent>().is_err());

        let empty_input = "";
        assert!(empty_input.parse::<LvIdent>().is_err());

        let invalid_input_with_dash = "vgname-";
        assert!(invalid_input_with_dash.parse::<LvIdent>().is_err());

        let invalid_input_with_double_dash = "vgname--";
        assert!(invalid_input_with_double_dash.parse::<LvIdent>().is_err());

        let invalid_input_with_leading_dash = "-lvname";
        assert!(invalid_input_with_leading_dash.parse::<LvIdent>().is_err());

        let invalid_input_with_leading_double_dash = "--vgname";
        assert!(
            invalid_input_with_leading_double_dash
                .parse::<LvIdent>()
                .is_err()
        );
    }
}
