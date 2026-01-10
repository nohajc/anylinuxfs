use std::path::Path;

use anyhow::{Context, anyhow};
use bstr::{BStr, BString, ByteSlice};
use common_utils::{PathExt, path_safe_label_name};
use libblkid_rs::{BlkidProbe, BlkidSublks, BlkidSublksFlags};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct DevInfo {
    path: BString,
    rpath: BString,
    block_size: Option<u32>,
    label: Option<String>,
    fs_type: Option<String>,
    uuid: Option<String>,
    vm_path: String,
    fs_driver: Option<String>, // will be auto-detected if not set
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
            vm_path: vm_path.into(),
            fs_driver: None,
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

        Ok(DevInfo {
            path,
            rpath,
            block_size,
            label,
            fs_type,
            uuid,
            vm_path: "/dev/vda".to_owned(),
            fs_driver: None,
        })
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

    pub fn vm_path(&self) -> &str {
        &self.vm_path
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
}
