use std::path::Path;

use anyhow::{Context, anyhow};
use libblkid_rs::{BlkidProbe, BlkidSublks, BlkidSublksFlags};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DevInfo {
    path: String,
    rpath: String,
    label: Option<String>,
    fs_type: Option<String>,
    uuid: Option<String>,
    vm_path: String,
    fs_driver: Option<String>, // will be auto-detected if not set
}

const BUF_PREFIX: &str = "/dev/disk";
const RAW_PREFIX: &str = "/dev/rdisk";

impl DevInfo {
    pub fn lv(
        path: &str,
        label: Option<&str>,
        vm_path: impl Into<String>,
    ) -> anyhow::Result<DevInfo> {
        Ok(DevInfo {
            path: path.into(),
            rpath: path.into(),
            label: label.map(|l| l.to_owned()),
            fs_type: Some("auto".into()),
            uuid: None,
            vm_path: vm_path.into(),
            fs_driver: None,
        })
    }

    pub fn pv(path: &str) -> anyhow::Result<DevInfo> {
        if path.is_empty() {
            return Err(anyhow!("Empty device path"));
        }
        let (path, rpath) = if path.starts_with(BUF_PREFIX) {
            (path.to_owned(), path.replace(BUF_PREFIX, RAW_PREFIX))
        } else if path.starts_with(RAW_PREFIX) {
            (path.replace(RAW_PREFIX, BUF_PREFIX), path.to_owned())
        } else {
            (path.to_owned(), path.to_owned())
        };

        let Ok(mut probe) = BlkidProbe::new_from_filename(Path::new(&path)) else {
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
            .enable_partitions(true)
            .context("Cannot enable device partition probe")?;
        probe
            .do_safeprobe()
            .context(format!("Cannot probe device {}", &path))?;

        if probe.get_partitions().is_ok() {
            return Err(anyhow!(
                "Device must be a single partition or filesystem image"
            ));
        }

        let label = probe.lookup_value("LABEL").ok();
        let fs_type = probe.lookup_value("TYPE").ok();
        let uuid = probe.lookup_value("UUID").ok();

        Ok(DevInfo {
            path,
            rpath,
            label,
            fs_type,
            uuid,
            vm_path: "/dev/vda".to_owned(),
            fs_driver: None,
        })
    }

    pub fn disk(&self) -> &str {
        &self.path
    }

    pub fn rdisk(&self) -> &str {
        &self.rpath
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

    pub fn auto_mount_name(&self) -> String {
        self.label()
            .map(|l| l.replace("/", "-").replace(" ", "_").replace(":", "_"))
            // .or(self.uuid())
            // .unwrap_or("lvol0")
            .unwrap_or(
                self.disk()
                    .split('/')
                    .last()
                    .map(|d| d.split(':').last())
                    .flatten()
                    .expect("non-empty disk path")
                    .into(),
            )
    }
}
