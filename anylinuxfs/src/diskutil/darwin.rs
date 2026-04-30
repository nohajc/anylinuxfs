use anyhow::Context;
use bstr::BStr;
use common_utils::host_println;
use objc2_core_foundation::{
    CFBoolean, CFDictionary, CFRetained, CFRunLoop, CFString, CFURL, kCFRunLoopDefaultMode,
};
use objc2_disk_arbitration::{
    DADisk, DARegisterDiskAppearedCallback, DARegisterDiskDisappearedCallback, DASession,
    DAUnregisterCallback,
};
use regex::Regex;
use serde::Deserialize;
use std::cmp;
use std::ffi::{CString, c_void};
use std::marker::PhantomData;
use std::path::Path;
use std::process::Command;
use std::ptr::{NonNull, null_mut};
use std::thread;
use url::Url;

use super::{DiskInfo, Entry, Labels, MountPoint, PartTypes, PvCollector, trunc_with_ellipsis};
use crate::devinfo::DevInfo;
use crate::fsutil;
use crate::pubsub::Subscription;
use crate::utils::cfdict_get_value;

pub(super) fn diskutil_list_from_plist(disk: Option<&str>) -> anyhow::Result<Plist> {
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

pub(super) fn disks_without_partition_table(plist: &Plist) -> Vec<String> {
    let mut disks = Vec::new();
    for disk in &plist.all_disks_and_partitions {
        if disk.partitions.is_none() && disk.content.as_deref() == Some("") {
            disks.push(disk.device_identifier.clone());
        }
    }
    disks
}

pub(super) fn partitions_with_part_type(plist: &Plist, part_types: &PartTypes) -> Vec<String> {
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

/// Drives `diskutil list` for one or more disks and translates its stdout
/// into `Entry` objects. Holds the two regexes used to parse the output so
/// they're compiled once per `list_partitions` call rather than once per
/// disk. Mirrors `linux::process_block_device` on the Linux side, but driven
/// by macOS's `diskutil` CLI output instead of sysfs.
pub(super) struct DiskUtilParser {
    numbered_pattern: Regex,
    part_type_pattern: Regex,
}

impl DiskUtilParser {
    pub(super) fn new(filter: &Labels) -> Self {
        Self {
            numbered_pattern: Regex::new(r"^\s+\d+:").unwrap(),
            part_type_pattern: Regex::new(&format!(r"({})", filter.part_types.join("|"))).unwrap(),
        }
    }

    /// Run `diskutil list` for `disk` (None means "all disks") and append one
    /// `Entry` per discovered disk to `disk_entries`.
    pub(super) fn process_disk(
        &self,
        disk: Option<&str>,
        filter: &Labels,
        pv: &mut PvCollector,
        disk_entries: &mut Vec<Entry>,
    ) -> anyhow::Result<()> {
        let plist_out = diskutil_list_from_plist(disk)?;
        let selected_partitions = partitions_with_part_type(&plist_out, &filter.part_types);
        let disks_without_part_table = disks_without_partition_table(&plist_out);

        let output = Command::new("diskutil")
            .arg("list")
            .args(disk)
            .output()
            .expect("Failed to execute diskutil");

        if !output.status.success() {
            anyhow::bail!("diskutil command failed");
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
            } else if self.numbered_pattern.is_match(line) {
                let Some(dev_ident) = line.split_whitespace().last() else {
                    continue;
                };
                if let Some(part_type) = self.part_type_pattern.find(line).map(|m| m.as_str()) {
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
        Ok(())
    }
}

pub(super) fn augment_line(
    line: &str,
    part_type: &str,
    dev_info: Option<&DevInfo>,
    fs_type: &str,
) -> String {
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

pub struct EventSession {
    session: CFRetained<DASession>,
}

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

        mount_ctx.real_mount_point.map(MountPoint::new)
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

#[derive(Debug, Deserialize)]
pub(super) struct Plist {
    #[serde(default, rename = "AllDisksAndPartitions")]
    all_disks_and_partitions: Vec<Disk>,
    #[serde(rename = "ErrorMessage")]
    error_message: Option<String>,
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
