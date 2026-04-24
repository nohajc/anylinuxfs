use anyhow::Context;
use bstr::BStr;
use common_utils::{PathExt, host_println, is_encrypted_fs, safe_print};
use derive_more::{AddAssign, Deref};
use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    cmp,
    fmt::Display,
    hash::{Hash, Hasher},
    io::{self, Write},
    iter,
    path::{Path, PathBuf},
    process::Command,
    str::FromStr,
    thread,
};

#[cfg(target_os = "macos")]
use std::{
    ffi::{CString, c_void},
    marker::PhantomData,
    ptr::{NonNull, null_mut},
};
#[cfg(target_os = "macos")]
use objc2_core_foundation::{
    CFBoolean, CFDictionary, CFRetained, CFRunLoop, CFString, CFURL, kCFRunLoopDefaultMode,
};
#[cfg(target_os = "macos")]
use objc2_disk_arbitration::{
    DADisk, DARegisterDiskAppearedCallback, DARegisterDiskDisappearedCallback, DASession,
    DAUnregisterCallback,
};
#[cfg(target_os = "macos")]
use url::Url;

use crate::{
    devinfo::DevInfo,
    pubsub::Subscription,
    settings::{Config, PassphrasePromptConfig},
    utils::is_stdin_tty,
    vm::{NetworkMode, VMOpts},
};
#[cfg(target_os = "macos")]
use crate::{
    fsutil,
    utils::cfdict_get_value,
};

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
        let entries_with_partitions: Vec<_> = self
            .0
            .iter()
            .filter(|e| !e.partitions().is_empty())
            .collect();

        for (idx, entry) in entries_with_partitions.iter().enumerate() {
            if idx > 0 {
                writeln!(f, "")?;
            }
            writeln!(f, "{}", entry.disk())?;
            if !entry.header().is_empty() {
                writeln!(f, "{}", entry.header())?;
            }
            if !entry.scheme().is_empty() {
                writeln!(f, "{}", entry.scheme())?;
            }
            for (pidx, partition) in entry.partitions().iter().enumerate() {
                let is_last_entry = idx == entries_with_partitions.len() - 1;
                let is_last_partition = pidx == entry.partitions().len() - 1;
                if is_last_entry && is_last_partition {
                    write!(f, "{}", partition)?;
                } else {
                    writeln!(f, "{}", partition)?;
                }
            }
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

fn normalize_pt_type(pt_type: &str) -> String {
    match pt_type {
        "gpt" => "GUID_partition_scheme".to_string(),
        "dos" => "FDisk_partition_scheme".to_string(),
        _ => pt_type.to_string(),
    }
}

fn format_partition_size(size_bytes: u64) -> String {
    const UNITS: &[&str] = &["", "K", "M", "G", "T", "P"];
    let mut size = size_bytes as f64;
    let mut unit_idx = 0;

    while size >= 1000.0 && unit_idx < UNITS.len() - 1 {
        size /= 1000.0;
        unit_idx += 1;
    }

    format!("{:.1} {}B", size, UNITS[unit_idx])
}

#[cfg(target_os = "macos")]
fn diskutil_list_from_plist(disk: Option<&str>) -> anyhow::Result<Plist> {
    let mut cmd = Command::new("diskutil");
    cmd.arg("list").arg("-plist");
    if let Some(d) = disk {
        cmd.arg(d);
    }
    let output = cmd.output().expect("Failed to execute diskutil");
    let plist: Plist = plist::from_bytes(&output.stdout).context("Failed to parse plist")?;

    if !output.status.success() {
        anyhow::bail!(
            "{}",
            plist
                .error_message
                .as_deref()
                .unwrap_or("diskutil command failed")
        );
    }

    Ok(plist)
}

#[cfg(target_os = "macos")]
fn disks_without_partition_table(plist: &Plist) -> Vec<String> {
    let mut disks = Vec::new();
    for disk in &plist.all_disks_and_partitions {
        if disk.partitions.is_none() && disk.content.as_deref() == Some("") {
            disks.push(disk.device_identifier.clone());
        }
    }
    disks
}

#[derive(Deref)]
pub struct PartTypes(&'static [&'static str]);

#[derive(Deref)]
pub struct FsTypes(&'static [&'static str]);

pub struct Labels {
    // normally, we match any filesystem with the following partition type
    pub part_types: PartTypes,
    // static fs list only used for matching drives without any partition table
    pub fs_types: FsTypes,
}

// Macro to define const string arrays without manually specifying the size
macro_rules! str_array {
    ($name:ident, [$($item:expr),* $(,)?]) => {
        const $name: [&str; { const COUNT: usize = [$(stringify!($item),)*].len(); COUNT }] = [
            $($item,)*
        ];
    };
}

// Const function to concatenate three arrays at compile time
const fn concat_str_arrays<
    'a,
    const N1: usize,
    const N2: usize,
    const N3: usize,
    const OUT: usize,
>(
    arr1: &[&'a str; N1],
    arr2: &[&'a str; N2],
    arr3: &[&'a str; N3],
) -> [&'a str; OUT] {
    let mut result = [""; OUT];
    let mut i = 0;
    while i < N1 {
        result[i] = arr1[i];
        i += 1;
    }
    let mut j = 0;
    while j < N2 {
        result[i + j] = arr2[j];
        j += 1;
    }
    let mut k = 0;
    while k < N3 {
        result[i + j + k] = arr3[k];
        k += 1;
    }
    result
}

// Individual label lists defined once
str_array!(
    LINUX_PART_TYPES,
    [
        "Linux Filesystem",
        "Linux LVM",
        "Linux_LVM",
        "Linux_RAID",
        "Linux",
        "ZFS",
        "0xE8",                                 // LUKS partition (MBR)
        "CA7D7CCB-63ED-4C53-861C-1742536059CC", // LUKS partition (GPT)
    ]
);

str_array!(
    LINUX_FS_TYPES,
    [
        "bcachefs",
        "btrfs",
        "erofs",
        "ext2",
        "ext3",
        "ext4",
        "f2fs",
        "squashfs",
        "xfs",
        "zfs",
        "crypto_LUKS",
        "linux_raid_member",
        "LVM2_member",
        "zfs_member",
    ]
);

// GPT - Microsoft Basic Data (any Windows filesystem)
// MBR - Windows_NTFS         (both NTFS and exFAT)
str_array!(
    WINDOWS_PART_TYPES,
    ["Microsoft Basic Data", "Windows_NTFS", "Windows_FAT_32"]
);

str_array!(WINDOWS_FS_TYPES, ["ntfs", "exfat", "BitLocker"]);

str_array!(
    BSD_PART_TYPES,
    [
        "FreeBSD UFS",
        "516E7CBA-6ECF-11D6-8FF8-00022D09712B" // FreeBSD ZFS
    ]
);

str_array!(BSD_FS_TYPES, ["ufs", "zfs"]);

const ALL_PART_TYPES: [&str;
    LINUX_PART_TYPES.len() + WINDOWS_PART_TYPES.len() + BSD_PART_TYPES.len()] =
    concat_str_arrays(&LINUX_PART_TYPES, &WINDOWS_PART_TYPES, &BSD_PART_TYPES);

const ALL_FS_TYPES: [&str; LINUX_FS_TYPES.len() + WINDOWS_FS_TYPES.len() + BSD_FS_TYPES.len()] =
    concat_str_arrays(&LINUX_FS_TYPES, &WINDOWS_FS_TYPES, &BSD_FS_TYPES);

pub const LINUX_LABELS: Labels = Labels {
    part_types: PartTypes(&LINUX_PART_TYPES),
    fs_types: FsTypes(&LINUX_FS_TYPES),
};

pub const WINDOWS_LABELS: Labels = Labels {
    part_types: PartTypes(&WINDOWS_PART_TYPES),
    fs_types: FsTypes(&WINDOWS_FS_TYPES),
};

pub const ALL_LABELS: Labels = Labels {
    part_types: PartTypes(&ALL_PART_TYPES),
    fs_types: FsTypes(&ALL_FS_TYPES),
};

#[cfg(target_os = "macos")]
fn partitions_with_part_type(plist: &Plist, part_types: &PartTypes) -> Vec<String> {
    let mut partitions = Vec::new();
    for disk in &plist.all_disks_and_partitions {
        if let Some(partitions_list) = &disk.partitions {
            for partition in partitions_list {
                if part_types
                    .iter()
                    .cloned()
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

    // replace in two steps
    // - part_type must be replaced with fs_type in any case
    // - label might already be there (for fs_types supported by macOS)
    let part_type_width = cmp::max(27, part_type.len() + 1);
    let width_diff = part_type_width - 27;
    line.replace(
        &format!("{part_type:>part_type_width$}"),
        &format!("{fs_type:>27}{:<width_diff$}", ""),
    )
    .replace(
        &format!("{:>27} {:<23}", fs_type, ""),
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
            anyhow::bail!("Invalid LV identifier: {}", s);
        }
        Ok(LvIdent { vg_name, lv_name })
    }
}

/// Accumulates physical volume entries (encrypted, RAID, LVM) discovered during partition listing.
struct PvCollector {
    dev_infos: Vec<DevInfo>,
    dev_idents: Vec<String>,
    enc_partitions: Vec<String>,
    assemble_raid: bool,
    decrypt_all: bool,
    has_enc_filter: bool,
}

impl PvCollector {
    fn new(enc_partitions: Option<&[String]>) -> Self {
        let decrypt_all = enc_partitions.is_some_and(|p| !p.is_empty() && p[0] == "all");
        Self {
            dev_infos: Vec::new(),
            dev_idents: Vec::new(),
            enc_partitions: Vec::new(),
            assemble_raid: false,
            decrypt_all,
            has_enc_filter: enc_partitions.is_some(),
        }
    }

    /// Check a partition's fs_type and accumulate it if it's a PV (encrypted, RAID, or LVM).
    /// Returns `(is_enc, is_raid, is_lvm)`.
    fn try_collect(
        &mut self,
        dev_info: &DevInfo,
        dev_ident: &str,
        disk_path: &str,
        fs_type: &str,
    ) -> (bool, bool, bool) {
        let is_enc = is_encrypted_fs(fs_type);
        let is_raid = fs_type == "linux_raid_member";
        let is_lvm = fs_type == "LVM2_member";

        if is_raid {
            self.assemble_raid = true;
        }

        if is_lvm || is_raid || (self.has_enc_filter && is_enc) {
            self.dev_infos.push(dev_info.to_owned());
            self.dev_idents.push(dev_ident.to_owned());

            if self.decrypt_all && is_enc {
                self.enc_partitions.push(disk_path.to_owned());
            }
        }

        (is_enc, is_raid, is_lvm)
    }
}

pub fn list_partitions(
    config: Config,
    disks: Option<&[String]>,
    enc_partitions: Option<&[String]>,
    filter: Labels,
) -> anyhow::Result<List> {
    let numbered_pattern = Regex::new(r"^\s+\d+:").unwrap();
    let part_type_pattern = Regex::new(&format!(r"({})", filter.part_types.join("|"))).unwrap();
    let mut disk_entries = Vec::new();

    let mut pv = PvCollector::new(enc_partitions);

    // Convert disks parameter to iterator: either None (single entry for all disks) or slice of devices
    let device_iter: Vec<Option<&str>> = match disks {
        None => vec![None], // Process all disks
        Some(slice) => slice.iter().map(|d| Some(d.as_str())).collect(), // Process each device
    };

    // Process each device (or all if None)
    for disk in device_iter {
        if let Some((path, p)) = disk.map(|d| (d, Path::new(d)))
            && p.exists()
            && p.is_file()
        {
            // It's an image file — probe directly with libblkid, bypassing diskutil.
            use bstr::BString;
            let probe_devs = DevInfo::probe_image(BString::from(p.as_bytes()))?;

            if !probe_devs.is_empty() {
                let whole = &probe_devs[0];
                let is_partitioned = whole.pt_type().is_some();

                let image_name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string());

                let mut entry = Entry::new(format!("{} (disk image):", path));
                entry.header_mut().push_str(
                    "   #:                       TYPE NAME                    SIZE       IDENTIFIER",
                );

                if is_partitioned {
                    let pt_type = whole.pt_type().unwrap_or("unknown");
                    let normalized_pt = normalize_pt_type(pt_type);
                    let whole_size = whole.size().map(format_partition_size).unwrap_or_default();
                    *entry.scheme_mut() = format!(
                        "   0: {:>26} {:<22} +{:<10} {}",
                        normalized_pt, "", whole_size, image_name,
                    );
                    for (i, dev_info) in probe_devs[1..].iter().enumerate() {
                        let fs_type = dev_info.fs_type().unwrap_or("");

                        // Filter by filesystem type to match diskutil behavior
                        if !filter.fs_types.iter().any(|t| t == &fs_type) {
                            continue;
                        }

                        pv.try_collect(dev_info, &image_name, path, fs_type);

                        let label = dev_info.label().unwrap_or("");
                        let truncated_label = trunc_with_ellipsis(label, 23);
                        let size_str = dev_info
                            .size()
                            .map(format_partition_size)
                            .unwrap_or_default();
                        let ident = format!("{}@s{}", image_name, i + 1);
                        entry.partitions_mut().push(format!(
                            "{:>4}: {:>26} {:<23} {:<10} {}",
                            i + 1,
                            fs_type,
                            truncated_label,
                            size_str,
                            ident,
                        ));
                    }
                } else {
                    // Whole-disk image without partition table
                    let dev_info = whole;
                    let fs_type = dev_info.fs_type().unwrap_or("");

                    // Filter by filesystem type to match diskutil behavior
                    if filter.fs_types.iter().any(|t| t == &fs_type) {
                        pv.try_collect(dev_info, &image_name, path, fs_type);

                        let label = dev_info.label().unwrap_or("");
                        let truncated_label = trunc_with_ellipsis(label, 23);
                        let size_str = dev_info
                            .size()
                            .map(format_partition_size)
                            .unwrap_or_default();
                        entry.partitions_mut().push(format!(
                            "   0: {:>26} {:<22} +{:<10} {}",
                            fs_type, truncated_label, size_str, image_name,
                        ));
                    }
                }

                disk_entries.push(entry);
            }
        } else {
            #[cfg(not(target_os = "macos"))]
            anyhow::bail!("listing physical block devices is not yet supported on this platform; pass an image file path instead");

            #[cfg(target_os = "macos")]
            let plist_out = diskutil_list_from_plist(disk)?;
            #[cfg(target_os = "macos")]
            let selected_partitions = partitions_with_part_type(&plist_out, &filter.part_types);
            #[cfg(target_os = "macos")]
            let disks_without_part_table = disks_without_partition_table(&plist_out);

            #[cfg(target_os = "macos")]
            let output = Command::new("diskutil")
                .arg("list")
                .args(disk)
                .output()
                .expect("Failed to execute diskutil");

            #[cfg(target_os = "macos")]
            if !output.status.success() {
                anyhow::bail!("diskutil command failed");
            }

            #[cfg(target_os = "macos")]
            let stdout = String::from_utf8_lossy(&output.stdout);
            #[cfg(target_os = "macos")]
            let mut current_entry = None;

            #[cfg(target_os = "macos")]
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
                        if !selected_partitions.iter().any(|p| p == dev_ident) {
                            continue;
                        }
                        let disk_path = format!("/dev/{dev_ident}");
                        let dev_info = DevInfo::pv(disk_path.as_str(), false).ok();

                        let line = match dev_info {
                            Some(dev_info) => {
                                let fs_type = dev_info.fs_type().unwrap_or(part_type);
                                pv.try_collect(&dev_info, dev_ident, &disk_path, fs_type);

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
                            let disk_path = format!("/dev/{dev_ident}");
                            let dev_info = DevInfo::pv(disk_path.as_str(), false).ok();

                            let fs_type = dev_info
                                .as_ref()
                                .map(|di| di.fs_type())
                                .flatten()
                                .unwrap_or("Unknown");
                            // if DevInfo is available, show linux fs types only
                            if fs_type != "Unknown"
                                && !filter.fs_types.iter().cloned().any(|t| t == fs_type)
                            {
                                continue;
                            }

                            if let Some(ref dev_info) = dev_info {
                                pv.try_collect(dev_info, dev_ident, &disk_path, fs_type);
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
        }
    }

    if !pv.dev_infos.is_empty() {
        let mut enc_partitions = enc_partitions;
        if pv.decrypt_all {
            enc_partitions = Some(&pv.enc_partitions);
        }
        match get_lsblk_info(&config, &pv.dev_infos, enc_partitions, pv.assemble_raid) {
            Ok(lsblk) => {
                if !lsblk.blockdevices.is_empty() {
                    let vol_map = VolumeMap::from_lsblk(&lsblk, &pv.dev_idents);
                    vol_map.build_entries(&mut disk_entries);
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
    RAID,
    BitLocker,
}

impl BlkDevKind {
    fn from_fstype(fstype: Option<&str>) -> Self {
        match fstype {
            Some("LVM2_member") => BlkDevKind::LVM,
            Some("crypto_LUKS") => BlkDevKind::LUKS,
            Some("linux_raid_member") => BlkDevKind::RAID,
            Some("BitLocker") => BlkDevKind::BitLocker,
            _ => BlkDevKind::Simple,
        }
    }
}

#[derive(Debug)]
struct VolumeMap {
    vol_groups: IndexMap<String, VgEntry>, // key: volume group name
    simple_enc_devs: IndexMap<String, LsBlkDevice>, // key: device identifier
    raid_volumes: IndexMap<String, RaidEntry>, // key: md name (for deduplication)
}

impl VolumeMap {
    fn new() -> Self {
        VolumeMap {
            vol_groups: IndexMap::new(),
            simple_enc_devs: IndexMap::new(),
            raid_volumes: IndexMap::new(),
        }
    }

    fn from_lsblk(lsblk: &LsBlk, pv_dev_idents: &[String]) -> Self {
        let mut vol_map = VolumeMap::new();

        fn iterate_children(
            vol_map: &mut VolumeMap,
            dev_ident: &str,
            dev_encrypted: bool,
            blkdev: &LsBlkDevice,
            kind: BlkDevKind,
            children: Option<&Vec<LsBlkDevice>>,
        ) {
            for (j, child) in children.into_iter().flatten().enumerate() {
                let child_kind = BlkDevKind::from_fstype(child.fstype.as_deref());

                if child_kind == BlkDevKind::Simple {
                    match kind {
                        BlkDevKind::LUKS | BlkDevKind::BitLocker => {
                            vol_map
                                .simple_enc_devs
                                .insert(dev_ident.into(), child.clone());
                        }
                        BlkDevKind::RAID => {
                            let RaidEntry {
                                dev_idents,
                                logical_vol,
                            } = vol_map.raid_volumes.entry(child.name.clone()).or_default();

                            *logical_vol = child.clone();
                            dev_idents.push(dev_ident.into());
                        }
                        BlkDevKind::LVM => {
                            if let Ok(lv_ident) = child.name.parse::<LvIdent>() {
                                let VgEntry {
                                    size,
                                    dev_idents,
                                    lvs,
                                    encrypted,
                                } = vol_map
                                    .vol_groups
                                    .entry(lv_ident.vg_name.to_string())
                                    .or_default();

                                if j == 0 {
                                    *size += parse_lv_size(&blkdev.size).unwrap_or(LvSize(0));

                                    dev_idents.push(dev_ident.into());
                                }

                                lvs.entry(child.clone()).or_default().push(dev_ident.into());

                                *encrypted = dev_encrypted;
                            }
                        }
                        _ => {}
                    }
                }

                iterate_children(
                    vol_map,
                    dev_ident,
                    dev_encrypted || kind == BlkDevKind::LUKS,
                    child,
                    child_kind,
                    child.children.as_ref(),
                );
            }
        }

        for (i, blkdev) in lsblk.blockdevices.iter().enumerate() {
            let dev_ident = &pv_dev_idents[i];
            let kind = BlkDevKind::from_fstype(blkdev.fstype.as_deref());
            let encrypted = kind == BlkDevKind::LUKS;

            iterate_children(
                &mut vol_map,
                dev_ident,
                encrypted,
                blkdev,
                kind,
                blkdev.children.as_ref(),
            )
        }

        vol_map
    }

    /// Build disk entries from RAID volumes, LVM volume groups, and encrypted device metadata.
    fn build_entries(&self, disk_entries: &mut Vec<Entry>) {
        for (
            _,
            RaidEntry {
                dev_idents,
                logical_vol,
            },
        ) in &self.raid_volumes
        {
            let mut entry = Entry::new("");
            entry.header_mut().push_str(
                "   #:                       TYPE NAME                    SIZE       IDENTIFIER",
            );

            let dev_ident = dev_idents.join(":");
            entry.partitions_mut().push(format!(
                "{:>4}: {:>26} {:<23} {:<10} {}",
                0,
                logical_vol.fstype.as_deref().unwrap_or(""),
                logical_vol.label.as_deref().unwrap_or(""),
                format_lv_size(&logical_vol.size),
                format!("{}", &dev_ident),
            ));

            *entry.disk_mut() = format!("raid:{} (volume):", &dev_ident);
            disk_entries.push(entry);
        }

        for (
            vg_name,
            VgEntry {
                size,
                dev_idents,
                lvs,
                encrypted: _,
            },
        ) in &self.vol_groups
        {
            let mut entry = Entry::new("");
            entry.header_mut().push_str(
                "   #:                       TYPE NAME                    SIZE       IDENTIFIER",
            );

            for (j, (child, devs)) in lvs.iter().enumerate() {
                let lv_ident = child.name.parse::<LvIdent>().unwrap();
                let dev_ident = devs.join(":");
                entry.partitions_mut().push(format!(
                    "{:>4}: {:>26} {:<23} {:<10} {}",
                    j + 1,
                    child.fstype.as_deref().unwrap_or(""),
                    child.label.as_deref().unwrap_or(""),
                    format_lv_size(&child.size),
                    format!("{}:{}:{}", &lv_ident.vg_name, &dev_ident, &lv_ident.lv_name),
                ));
            }

            if !entry.partitions().is_empty() {
                *entry.disk_mut() = format!("lvm:{} (volume group):", &vg_name);
                *entry.scheme_mut() = format!(
                    "   0:                LVM2_scheme                        +{:<10} {}",
                    size, &vg_name
                );

                let mut label = "Physical Store";
                for dev_ident in dev_idents {
                    *entry.scheme_mut() += &format!("\n{:<32} {} {}", "", label, dev_ident);
                    label = "              ";
                }

                disk_entries.push(entry);
            }
        }

        // extend entries with decrypted metadata
        for entry in disk_entries {
            for part in entry.partitions_mut() {
                for enc_type in ["crypto_LUKS", "BitLocker"] {
                    if part.contains(enc_type) {
                        if let Some(dev_ident) = part.split_whitespace().last() {
                            if let Some(enc_dev) = self.simple_enc_devs.get(dev_ident) {
                                if let Some(fstype) = enc_dev.fstype.as_deref() {
                                    let enc_fs_type = format!("{}: {}", enc_type, fstype);
                                    *part = part
                                        .replace(
                                            &format!("{:>27}", enc_type),
                                            &format!("{:>27}", enc_fs_type),
                                        )
                                        .replace(
                                            &format!("{:>27} {:<23}", enc_fs_type, ""),
                                            &format!(
                                                "{:>27} {:<23}",
                                                enc_fs_type,
                                                enc_dev.label.as_deref().unwrap_or(""),
                                            ),
                                        );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn read_passphrase(partition: Option<&str>) -> anyhow::Result<String> {
    let text = match partition {
        Some(part) => format!("Enter passphrase for {}: ", part),
        None => "Enter passphrase: ".to_string(),
    };
    Ok(rpassword::prompt_password(text).context("Failed to read passphrase")?)
}

fn write_passphrase_to_pipe(in_fd: libc::c_int, passphrase: &str) -> anyhow::Result<()> {
    Ok(
        unsafe { crate::write_to_pipe(in_fd, format!("{passphrase}\n").as_bytes()) }
            .context("Failed to write to pipe")?,
    )
}

fn passphrase_prompt_lazy(
    partition: Option<&str>,
) -> impl Fn(libc::c_int, usize) -> anyhow::Result<()> {
    move |in_fd, pwd_reps| {
        // prompt user for passphrase
        let passphrase = read_passphrase(partition)?;
        for _ in 0..pwd_reps {
            write_passphrase_to_pipe(in_fd, &passphrase)?;
        }

        Ok(())
    }
}

pub fn passphrase_prompt(partition: Option<PathBuf>) -> impl FnOnce() {
    move || {
        if !is_stdin_tty() {
            return;
        }
        match partition {
            Some(part) => {
                _ = safe_print!("Enter passphrase for {}: ", part.display());
            }
            None => {
                _ = safe_print!("Enter passphrase: ");
            }
        }
        io::stdout().flush().unwrap_or(());
    }
}

fn virt_disk_to_decrypt(dev_info: &[DevInfo], partition: &str) -> anyhow::Result<(String, String)> {
    let enc_part_idx = dev_info
        .iter()
        .position(|di| di.disk() == Path::new(partition));
    Ok(match enc_part_idx {
        Some(idx) => (
            format!(
                "/dev/vd{}",
                ('a'..='z')
                    .nth(idx)
                    .context("block device index out of range")?
            ),
            dev_info[idx]
                .fs_type()
                .context("missing fs_type info")?
                .into(),
        ),
        None => anyhow::bail!("Partition {} not found", partition),
    })
}

fn decrypt_script(dev_info: &[DevInfo], partitions: Option<&[String]>) -> anyhow::Result<String> {
    let Some(partitions) = partitions else {
        return Ok(String::new());
    };

    let mut script = String::new();

    for (i, part) in partitions.iter().enumerate() {
        let (vdev_path, fs_type) = virt_disk_to_decrypt(dev_info, part)?;
        match fs_type.as_str() {
            "crypto_LUKS" => script += &format!("cryptsetup open {} luks{i}; ", vdev_path),
            "BitLocker" => script += &format!("cryptsetup bitlkOpen {} btlk{i}; ", vdev_path),
            _ => (),
        }
    }

    Ok(script)
}

fn get_lsblk_info(
    config: &Config,
    dev_info: &[DevInfo],
    enc_partitions: Option<&[String]>,
    assemble_raid: bool,
) -> anyhow::Result<LsBlk> {
    let prelude = "mount -t tmpfs tmpfs /tmp && \
        mount -t tmpfs tmpfs /run && ";
    let script = format!(
        "{prelude}{}{}/sbin/vgchange -ay >/dev/null; /bin/lsblk -O --json",
        decrypt_script(dev_info, enc_partitions)?,
        if assemble_raid {
            "/sbin/mdadm --assemble --scan 2>/dev/null; "
        } else {
            ""
        }
    );
    let lsblk_args = [
        "/bin/busybox".into(),
        "sh".into(),
        "-c".into(),
        script.into(),
    ];
    let prompt_fn = enc_partitions.map(|partitions| {
        let mut passphrase_prompts = Vec::new();
        let pwd_reps = match config.passphrase_config {
            PassphrasePromptConfig::AskForEach => {
                for part in partitions {
                    passphrase_prompts.push(passphrase_prompt_lazy(Some(part)));
                }
                1
            }
            PassphrasePromptConfig::OneForAll => {
                passphrase_prompts.push(passphrase_prompt_lazy(None));
                partitions.len()
            }
        };
        move |in_fd: libc::c_int| -> anyhow::Result<()> {
            for passphrase_fn in passphrase_prompts {
                passphrase_fn(in_fd, pwd_reps)?;
            }
            Ok(())
        }
    });
    let lsblk_cmd = crate::vm::run_vmcommand_short(
        config,
        dev_info,
        NetworkMode::Default,
        VMOpts::new()
            .read_only_disks(true)
            .read_only_root(!config.rw_rootfs),
        &lsblk_args,
        prompt_fn,
    )
    .context("Failed to run command in microVM")?;
    if lsblk_cmd.status != 0 {
        anyhow::bail!("lsblk command failed");
    }

    eprintln!("{}", String::from_utf8_lossy(&lsblk_cmd.stderr));

    let lsblk = serde_json::from_slice(&lsblk_cmd.stdout)
        .context("failed to parse lsblk command output")?;

    Ok(lsblk)
}

#[cfg(target_os = "macos")]
struct DaDiskArgs<ContextType> {
    context: *mut c_void,
    descr: Option<CFRetained<CFDictionary>>,
    phantom: PhantomData<ContextType>,
}

#[cfg(target_os = "macos")]
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
        self.descr
            .as_ref()
            .map(|d| unsafe { CFRetained::as_ptr(d).as_ref() })
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

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
struct MountContext<'a> {
    nfs_path: &'a Path,
    // what the OS assigned after resolving any potential conflicts
    real_mount_point: Option<String>,
}

#[cfg(target_os = "macos")]
impl<'a> MountContext<'a> {
    fn new(nfs_path: &'a Path) -> Self {
        Self {
            nfs_path,
            real_mount_point: None,
        }
    }
}

#[cfg(target_os = "macos")]
struct UnmountContext<'a> {
    mount_point: &'a str,
}

#[cfg(target_os = "macos")]
fn stop_run_loop_on_signal(signals: Subscription<libc::c_int>) -> anyhow::Result<()> {
    _ = thread::spawn(move || {
        for _ in signals {
            host_println!("Termination requested, give up waiting for mount");
            CFRunLoop::stop(&CFRunLoop::main().unwrap());
            break;
        }
    });

    Ok(())
}

pub struct MountPoint(String);

impl MountPoint {
    pub fn real(&self) -> &str {
        self.0.as_str()
    }

    // On macOS, disk events contain mount points with trailing slashes;
    // trim them for display purposes on all platforms.
    pub fn display(&self) -> &str {
        self.0.as_str().trim_end_matches('/')
    }
}

#[cfg(target_os = "macos")]
pub struct EventSession {
    session: CFRetained<DASession>,
}

#[cfg(target_os = "macos")]
impl EventSession {
    pub fn new(signals: Subscription<libc::c_int>) -> anyhow::Result<Self> {
        let session = unsafe { DASession::new(None).unwrap() };
        stop_run_loop_on_signal(signals)?;
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

// Linux EventSession: polls /proc/mounts to detect NFS share mount/unmount.
// mount(8) is synchronous on Linux so wait_for_mount normally returns quickly.
#[cfg(not(target_os = "macos"))]
pub struct EventSession {
    signals: Subscription<libc::c_int>,
}

#[cfg(not(target_os = "macos"))]
impl EventSession {
    pub fn new(signals: Subscription<libc::c_int>) -> anyhow::Result<Self> {
        Ok(Self { signals })
    }

    // Returns None when interrupted by SIGINT/SIGTERM.
    pub fn wait_for_mount(&self, nfs_path: &Path) -> Option<MountPoint> {
        let target = nfs_path.to_string_lossy().into_owned();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if self.signals.try_recv().is_ok() {
                host_println!("Termination requested, give up waiting for mount");
                return None;
            }
            if let Ok(mp) = find_mount_point_in_proc_mounts(&target) {
                return Some(MountPoint(mp));
            }
            if std::time::Instant::now() > deadline {
                return None;
            }
            thread::sleep(std::time::Duration::from_millis(200));
        }
    }

    pub fn wait_for_unmount(&self, mount_point: &str) {
        loop {
            if self.signals.try_recv().is_ok() {
                host_println!("Termination requested");
                return;
            }
            if !is_mounted_at(mount_point) {
                return;
            }
            thread::sleep(std::time::Duration::from_millis(500));
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn find_mount_point_in_proc_mounts(nfs_from: &str) -> io::Result<String> {
    use std::io::BufRead;
    let file = std::fs::File::open("/proc/mounts")?;
    for line in io::BufReader::new(file).lines() {
        let line = line?;
        let mut fields = line.splitn(3, ' ');
        let from = fields.next().unwrap_or("");
        let on = fields.next().unwrap_or("");
        if from == nfs_from {
            return Ok(on.to_owned());
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "not mounted yet"))
}

#[cfg(not(target_os = "macos"))]
fn is_mounted_at(mount_point: &str) -> bool {
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open("/proc/mounts") else {
        return false;
    };
    for line in io::BufReader::new(file).lines().flatten() {
        let mut fields = line.splitn(3, ' ');
        let _ = fields.next();
        let on = fields.next().unwrap_or("");
        if on == mount_point {
            return true;
        }
    }
    false
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DiskInfo {
    pub media_writable: bool,
}

impl Default for DiskInfo {
    fn default() -> Self {
        Self {
            media_writable: true,
        }
    }
}

#[cfg(target_os = "macos")]
pub fn get_info(bsd_name: impl AsRef<BStr>) -> DiskInfo {
    let session = unsafe { DASession::new(None).unwrap() };
    let c_bsd_name = CString::new(bsd_name.as_ref().to_owned()).unwrap();

    let media_writable = match unsafe {
        DADisk::from_bsd_name(
            None,
            &session,
            NonNull::new_unchecked(c_bsd_name.into_raw()),
        )
    } {
        Some(disk) => match unsafe { DADisk::description(disk.as_ref()) } {
            Some(descr) => {
                let media_writable: Option<&CFBoolean> =
                    unsafe { cfdict_get_value(&descr, "DAMediaWritable") };

                media_writable.map(|b| b.value()).unwrap_or(true)
            }
            None => true,
        },
        None => true,
    };

    DiskInfo { media_writable }
}

#[cfg(not(target_os = "macos"))]
pub fn get_info(dev_path: impl AsRef<BStr>) -> DiskInfo {
    // Read /sys/block/<name>/ro: "1" means read-only.
    let name = std::path::Path::new(std::ffi::OsStr::from_bytes(dev_path.as_ref().as_bytes()))
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ro_path = format!("/sys/block/{}/ro", name);
    let media_writable = std::fs::read_to_string(&ro_path)
        .map(|s| s.trim() == "0")
        .unwrap_or(true);
    DiskInfo { media_writable }
}

#[cfg(target_os = "macos")]
#[derive(Debug, Deserialize)]
struct Plist {
    #[serde(default, rename = "AllDisksAndPartitions")]
    all_disks_and_partitions: Vec<Disk>,
    #[serde(rename = "ErrorMessage")]
    error_message: Option<String>,
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
#[allow(unused)]
#[derive(Debug, Deserialize)]
struct PhysicalStore {
    #[serde(rename = "DeviceIdentifier")]
    device_identifier: String,
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
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
    size: LvSize,
    dev_idents: Vec<String>,
    lvs: IndexMap<LsBlkDevice /* lv_uuid */, Vec<String /* dev_ident */>>,
    encrypted: bool,
}

impl Default for VgEntry {
    fn default() -> Self {
        Self {
            size: LvSize(0),
            dev_idents: Vec::new(),
            lvs: IndexMap::new(),
            encrypted: false,
        }
    }
}

#[derive(Debug, Default)]
struct RaidEntry {
    dev_idents: Vec<String>,
    logical_vol: LsBlkDevice,
}

#[allow(unused)]
#[derive(Debug, Default, Deserialize, Clone)]
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
