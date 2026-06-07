use bstr::{BStr, BString, ByteSlice};
use common_utils::host_println;
use std::io;
use std::path::Path;
use std::thread;

use super::{
    DiskInfo, Entry, Labels, MountPoint, PvCollector, entry_with_header, format_partition_row,
    format_partition_size, format_prefixed_row, normalize_pt_type, trunc_with_ellipsis,
};
use crate::devinfo::DevInfo;
use crate::pubsub::Subscription;

pub(super) fn is_supported_disk_name(name: &str) -> bool {
    if name.starts_with("sd")
        || name.starts_with("vd")
        || name.starts_with("xvd")
        || name.starts_with("hd")
    {
        // sd/vd/xvd/hd: must be followed by a letter, not a digit
        // (rules out artifacts like sda1 if anything ever shows up at top level)
        return name
            .chars()
            .nth(if name.starts_with("xvd") { 3 } else { 2 })
            .map(|c| c.is_ascii_alphabetic())
            .unwrap_or(false);
    }
    if name.starts_with("nvme") {
        // nvme<ctrl>n<ns> — keep only namespace nodes (no trailing partition)
        return name.contains('n') && !name.contains('p');
    }
    if name.starts_with("mmcblk") {
        // mmcblk<N> (no trailing 'p<part>')
        return !name.contains('p');
    }
    if let Some(rest) = name.strip_prefix("loop") {
        // loop[0-9]+ — but only consider it "supported" when actually
        // backed by a file. The kernel preallocates loop0..loop7 idle.
        return !rest.is_empty()
            && rest.chars().all(|c| c.is_ascii_digit())
            && loop_backing_file(name).is_some();
    }
    false
}

// Path of the file backing a loop device, or None if the device is idle.
// `/sys/block/loopN/loop/backing_file` only exists once losetup has
// associated a file; for idle preallocated loops the `loop/` subdir is
// absent.
fn loop_backing_file(disk_name: &str) -> Option<String> {
    let p = std::path::PathBuf::from(format!("/sys/block/{}/loop/backing_file", disk_name));
    let s = std::fs::read_to_string(&p).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(super) fn enumerate_physical_disks() -> Vec<String> {
    let entries = match std::fs::read_dir("/sys/block") {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| is_supported_disk_name(n))
        .collect();
    names.sort();
    names
}

fn read_sectors(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
}

fn read_disk_size(disk_name: &str) -> Option<u64> {
    let p = std::path::PathBuf::from(format!("/sys/block/{}/size", disk_name));
    read_sectors(&p).map(|sectors| sectors * 512)
}

fn read_part_size(disk_name: &str, part_name: &str) -> Option<u64> {
    let p = std::path::PathBuf::from(format!("/sys/block/{}/{}/size", disk_name, part_name));
    read_sectors(&p).map(|sectors| sectors * 512)
}

fn is_removable(disk_name: &str) -> bool {
    let p = std::path::PathBuf::from(format!("/sys/block/{}/removable", disk_name));
    std::fs::read_to_string(&p)
        .map(|s| s.trim() == "1")
        .unwrap_or(false)
}

// Partition subdirs under `/sys/block/<disk>/`. Each subdir that has a
// `partition` file is a partition; the file's content is the partition number.
// Returned ordered by partition number (so output matches the partition table
// even if sysfs hands us subdirs in inode order).
fn list_partition_names_sysfs(disk_name: &str) -> Vec<String> {
    let dir = std::path::PathBuf::from(format!("/sys/block/{}", disk_name));
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut parts: Vec<(u64, String)> = Vec::new();
    for entry in entries.flatten() {
        let name = match entry.file_name().into_string() {
            Ok(n) => n,
            Err(_) => continue,
        };
        let part_num_path = entry.path().join("partition");
        let Some(num) = read_sectors(&part_num_path) else {
            continue;
        };
        parts.push((num, name));
    }
    parts.sort_by_key(|(n, _)| *n);
    parts.into_iter().map(|(_, n)| n).collect()
}

// Build an Entry for a single Linux block device using sysfs as the
// structural source of truth and libblkid for fs metadata. Returns None if
// no rows survive the filter (so the caller can skip empty disks).
pub(super) fn process_block_device(
    disk_path: &str,
    filter: &Labels,
    pv: &mut PvCollector,
) -> Option<Entry> {
    let disk_name = std::path::Path::new(disk_path)
        .file_name()?
        .to_str()?
        .to_string();

    // Loop devices are the Linux analogue of macOS hdiutil-attached images;
    // mirror diskutil's "disk image" tag for them — same as the image-mode
    // path uses for file-based images. Physical block devices keep the
    // "physical" tag with an internal/external prefix driven by the
    // kernel's `removable` flag.
    let header_tag = if disk_name.starts_with("loop") {
        "disk image".to_string()
    } else {
        let location = if is_removable(&disk_name) {
            "external"
        } else {
            "internal"
        };
        format!("{}, physical", location)
    };
    let mut entry = entry_with_header(format!("{} ({}):", disk_path, header_tag));

    // Whole-disk size prefix mirrors macOS diskutil: `*` for physical block
    // devices, `+` for disk images (loop / hdiutil-attached).
    let size_prefix = if disk_name.starts_with("loop") {
        '+'
    } else {
        '*'
    };

    // Whole-disk libblkid probe — gives PT type, fs type (for whole-disk
    // FS), and labels. Failure (e.g. EACCES without sudo) is fine: we still
    // show structure from sysfs.
    let probe = DevInfo::probe_image(BString::from(disk_path.as_bytes())).ok();
    let whole = probe.as_ref().and_then(|p| p.first());

    let pt_type = whole.and_then(|w| w.pt_type());
    let whole_fs_type = whole.and_then(|w| w.fs_type());
    let whole_size = whole
        .and_then(|w| w.size())
        .or_else(|| read_disk_size(&disk_name));

    if let Some(pt) = pt_type {
        // Partitioned disk
        let normalized_pt = normalize_pt_type(pt);
        let size_str = whole_size.map(format_partition_size).unwrap_or_default();
        *entry.scheme_mut() =
            format_prefixed_row(0, &normalized_pt, "", size_prefix, &size_str, &disk_name);

        let part_names = list_partition_names_sysfs(&disk_name);
        for (i, part_name) in part_names.iter().enumerate() {
            let part_path = format!("/dev/{}", part_name);
            let part_info = DevInfo::pv(part_path.as_str(), false).ok();
            let fs_type = part_info.as_ref().and_then(|p| p.fs_type()).unwrap_or("");

            // Filter by fs type (matches macOS image-mode behaviour: empty
            // fs_type means libblkid couldn't read it — keep the row in
            // that case so the user sees structure).
            if !fs_type.is_empty() && !filter.fs_types.iter().any(|t| t == &fs_type) {
                continue;
            }

            if let Some(ref dev_info) = part_info {
                pv.try_collect(dev_info, part_name, &part_path, fs_type);
            }

            let label = part_info.as_ref().and_then(|p| p.label()).unwrap_or("");
            let truncated_label = trunc_with_ellipsis(label, 23);
            let part_size = read_part_size(&disk_name, part_name);
            let size_str = part_size.map(format_partition_size).unwrap_or_default();
            entry.partitions_mut().push(format_partition_row(
                i + 1,
                fs_type,
                &truncated_label,
                &size_str,
                part_name,
            ));
        }
    } else if let Some(fs_type) = whole_fs_type {
        // Whole-disk filesystem (no partition table)
        if !filter.fs_types.iter().any(|t| t == &fs_type) {
            return None;
        }
        if let Some(w) = whole {
            pv.try_collect(w, &disk_name, disk_path, fs_type);
        }
        let label = whole.and_then(|w| w.label()).unwrap_or("");
        let truncated_label = trunc_with_ellipsis(label, 23);
        let size_str = whole_size.map(format_partition_size).unwrap_or_default();
        entry.partitions_mut().push(format_prefixed_row(
            0,
            fs_type,
            &truncated_label,
            size_prefix,
            &size_str,
            &disk_name,
        ));
    } else {
        // No probe info (unprivileged or unknown content). Show structure
        // from sysfs only — partition list with sizes, no fs/label.
        let part_names = list_partition_names_sysfs(&disk_name);
        if part_names.is_empty() {
            return None;
        }
        let size_str = whole_size.map(format_partition_size).unwrap_or_default();
        *entry.scheme_mut() = format_prefixed_row(0, "", "", size_prefix, &size_str, &disk_name);
        for (i, part_name) in part_names.iter().enumerate() {
            let part_size = read_part_size(&disk_name, part_name);
            let size_str = part_size.map(format_partition_size).unwrap_or_default();
            entry
                .partitions_mut()
                .push(format_partition_row(i + 1, "", "", &size_str, part_name));
        }
    }

    Some(entry)
}

/// Linux EventSession: polls /proc/mounts to detect NFS share mount/unmount.
/// mount(8) is synchronous on Linux so wait_for_mount normally returns quickly.
pub struct EventSession {
    signals: Subscription<libc::c_int>,
}

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
                return Some(MountPoint::new(mp));
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

fn find_mount_point_in_proc_mounts(nfs_from: &str) -> io::Result<String> {
    let mounts =
        procfs::mounts().map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    for entry in mounts {
        if entry.fs_spec == nfs_from {
            return Ok(entry.fs_file);
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "not mounted yet"))
}

fn is_mounted_at(mount_point: &str) -> bool {
    let Ok(mounts) = procfs::mounts() else {
        return false;
    };
    mounts.iter().any(|entry| entry.fs_file == mount_point)
}

pub fn get_info(dev_path: impl AsRef<BStr>) -> DiskInfo {
    use std::os::unix::ffi::OsStrExt;
    let bytes: &[u8] = dev_path.as_ref().as_bytes();
    let name = std::path::Path::new(std::ffi::OsStr::from_bytes(bytes))
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ro_path = format!("/sys/class/block/{}/ro", name);
    let media_writable = std::fs::read_to_string(&ro_path)
        .map(|s| s.trim() == "0")
        .unwrap_or(true);
    DiskInfo { media_writable }
}
