use std::os::fd::AsRawFd;
use std::path::Path;

use anyhow::{Context, anyhow};
use bstr::{BStr, BString, ByteSlice};
use common_utils::{PathExt, path_safe_label_name};
use libblkid_rs::{BlkidProbe, BlkidSublks, BlkidSublksFlags};
use serde::{Deserialize, Serialize};

use crate::diskutil;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct DevInfo {
    path: BString,
    rpath: BString,
    block_size: Option<u32>,
    label: Option<String>,
    fs_type: Option<String>,
    uuid: Option<String>,
    pt_type: Option<String>,
    vm_path: String,
    fs_driver: Option<String>, // will be auto-detected if not set
    da_info: diskutil::DiskInfo,
    size_bytes: Option<u64>, // partition size in bytes (for image probes)
}

const BUF_PREFIX: &[u8] = b"/dev/disk";
const RAW_PREFIX: &[u8] = b"/dev/rdisk";

impl DevInfo {
    pub fn lv(
        path: &str,
        label: Option<&str>,
        vm_path: impl Into<String>,
    ) -> anyhow::Result<DevInfo> {
        Ok(DevInfo {
            path: path.into(),
            rpath: path.into(),
            block_size: None,
            label: label.map(|l| l.to_owned()),
            fs_type: Some("auto".into()),
            uuid: None,
            pt_type: None,
            vm_path: vm_path.into(),
            fs_driver: None,
            da_info: diskutil::DiskInfo::default(),
            size_bytes: None,
        })
    }

    pub fn pv(path: impl AsRef<BStr>) -> anyhow::Result<DevInfo> {
        let path = path.as_ref();
        if path.is_empty() {
            return Err(anyhow!("Empty device path"));
        }
        let (path, rpath) = if path.starts_with(BUF_PREFIX) {
            (
                path.to_owned(),
                BString::new(path.replace(BUF_PREFIX, RAW_PREFIX)),
            )
        } else if path.starts_with(RAW_PREFIX) {
            (path.replace(RAW_PREFIX, BUF_PREFIX).into(), path.to_owned())
        } else {
            (path.to_owned(), path.to_owned())
        };

        let Ok(mut probe) = BlkidProbe::new_from_filename(Path::from_bytes(&path)) else {
            return Err(anyhow!("Cannot probe device. Insufficient permissions?"));
        };
        probe
            .enable_superblocks(true)
            .context("Cannot enable device superblock probe")?;
        probe
            .set_superblock_flags(BlkidSublksFlags::new(vec![
                BlkidSublks::Label,
                BlkidSublks::Type,
                BlkidSublks::Uuid,
            ]))
            .context("Cannot configure device superblock probe")?;
        probe
            .do_safeprobe()
            .context(format!("Cannot probe device {}", &path))?;

        let block_size = probe
            .lookup_value("BLOCK_SIZE")
            .ok()
            .and_then(|v| v.parse().ok());

        let label = probe.lookup_value("LABEL").ok();
        let fs_type = probe.lookup_value("TYPE").ok();
        let uuid = probe.lookup_value("UUID").ok();

        // also get info from DiskArbitration
        let da_info = diskutil::get_info(&path);

        Ok(DevInfo {
            path,
            rpath,
            block_size,
            label,
            fs_type,
            uuid,
            pt_type: None,
            vm_path: "/dev/vda".to_owned(),
            fs_driver: None,
            da_info,
            size_bytes: None,
        })
    }

    pub fn probe_image(path: impl AsRef<BStr>) -> anyhow::Result<Vec<DevInfo>> {
        let path = path.as_ref();
        if path.is_empty() {
            return Err(anyhow::anyhow!("Empty image path"));
        }

        let path_ref = Path::from_bytes(path);

        let mut whole_probe =
            BlkidProbe::new_from_filename(path_ref).context("Cannot open image with BlkidProbe")?;

        whole_probe
            .enable_partitions(true)
            .context("Cannot enable partitions probe")?;

        whole_probe
            .enable_superblocks(true)
            .context("Cannot enable superblocks probe")?;

        whole_probe
            .set_superblock_flags(BlkidSublksFlags::new(vec![
                BlkidSublks::Label,
                BlkidSublks::Type,
                BlkidSublks::Uuid,
            ]))
            .context("Cannot configure superblock probe")?;

        whole_probe
            .do_safeprobe()
            .context(format!("Cannot probe image {}", path.as_bstr()))?;

        let block_size = whole_probe
            .lookup_value("BLOCK_SIZE")
            .ok()
            .and_then(|v| v.parse().ok());

        let label = whole_probe.lookup_value("LABEL").ok();
        let fs_type = whole_probe.lookup_value("TYPE").ok();
        let uuid = whole_probe.lookup_value("UUID").ok();
        let pt_type = whole_probe.lookup_value("PTTYPE").ok();

        let mut result = vec![DevInfo {
            path: path.to_owned(),
            rpath: path.to_owned(),
            block_size,
            label,
            fs_type,
            uuid,
            pt_type,
            vm_path: "/dev/vda".to_owned(),
            fs_driver: None,
            da_info: diskutil::DiskInfo::default(),
            size_bytes: Some(whole_probe.get_size() as u64),
        }];

        if let Ok(mut partitions) = whole_probe.get_partitions() {
            if let Ok(num_parts) = partitions.number_of_partitions() {
                let file = std::fs::File::open(path_ref)
                    .context(format!("Cannot open image {}", path.as_bstr()))?;
                let fd = file.as_raw_fd();

                for i in 0..num_parts {
                    let part = partitions
                        .get_partition(i)
                        .context(format!("Cannot get partition {}", i))?;

                    let start = part.get_start();
                    let size = part.get_size();

                    let offset_bytes = (*start.as_ref()) as i64 * 512;
                    let size_bytes = (*size.as_ref()) as i64 * 512;

                    let mut part_probe =
                        BlkidProbe::new().context("Cannot create partition probe")?;

                    let _ = part_probe.set_device(fd, offset_bytes, size_bytes);
                    let _ = part_probe.enable_superblocks(true);
                    let _ = part_probe.set_superblock_flags(BlkidSublksFlags::new(vec![
                        BlkidSublks::Label,
                        BlkidSublks::Type,
                        BlkidSublks::Uuid,
                    ]));
                    let _ = part_probe.do_safeprobe();

                    let part_block_size = part_probe
                        .lookup_value("BLOCK_SIZE")
                        .ok()
                        .and_then(|v| v.parse().ok());

                    let part_label = part_probe.lookup_value("LABEL").ok();
                    let part_fs_type = part_probe.lookup_value("TYPE").ok();
                    let part_uuid = part_probe.lookup_value("UUID").ok();

                    let size_bytes = Some(size_bytes as u64);
                    result.push(DevInfo {
                        // partition path doesn't really exist for images
                        // so we always pass the whole disk to the microVM
                        path: path.into(),
                        rpath: path.into(),
                        block_size: part_block_size,
                        label: part_label,
                        fs_type: part_fs_type,
                        uuid: part_uuid,
                        pt_type: None,
                        vm_path: format!("/dev/vda{}", i + 1),
                        fs_driver: None,
                        da_info: diskutil::DiskInfo::default(),
                        size_bytes,
                    });
                }
            }
        }

        Ok(result)
    }

    pub fn disk(&self) -> &Path {
        Path::from_bytes(&self.path)
    }

    pub fn rdisk(&self) -> &Path {
        Path::from_bytes(&self.rpath)
    }

    pub fn block_size(&self) -> Option<u32> {
        self.block_size
    }

    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    pub fn set_label(&mut self, label: &str) {
        self.label = Some(label.to_owned());
    }

    pub fn fs_type(&self) -> Option<&str> {
        self.fs_type.as_deref()
    }

    pub fn fs_driver(&self) -> Option<&str> {
        self.fs_driver.as_deref().or(self.fs_type.as_deref())
    }

    pub fn set_fs_type(&mut self, fs_type: &str) {
        self.fs_type = Some(fs_type.to_owned());
    }

    pub fn set_fs_driver(&mut self, fs_driver: &str) {
        self.fs_driver = Some(fs_driver.to_owned());
    }

    pub fn uuid(&self) -> Option<&str> {
        self.uuid.as_deref()
    }

    pub fn pt_type(&self) -> Option<&str> {
        self.pt_type.as_deref()
    }

    pub fn size(&self) -> Option<u64> {
        self.size_bytes
    }

    pub fn vm_path(&self) -> &str {
        &self.vm_path
    }

    pub fn set_vm_path(&mut self, vm_path: String) {
        self.vm_path = vm_path;
    }

    pub fn auto_mount_name(&self) -> BString {
        self.label()
            .and_then(path_safe_label_name)
            .map(BString::from)
            // .or(self.uuid())
            // .unwrap_or("lvol0")
            .unwrap_or(
                self.disk()
                    .as_bytes()
                    .split(|&c| c == b'/')
                    .last()
                    .map(|d| d.split(|&c| c == b':').last())
                    .flatten()
                    .expect("non-empty disk path")
                    .into(),
            )
    }

    pub fn media_writable(&self) -> bool {
        self.da_info.media_writable
    }
}
