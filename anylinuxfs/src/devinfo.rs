use std::path::Path;

use anyhow::Context;
use libblkid_rs::{BlkidProbe, BlkidSublks, BlkidSublksFlags};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DevInfo {
    path: String,
    rpath: String,
    label: Option<String>,
    fs_type: Option<String>,
    uuid: Option<String>,
}

const BUF_PREFIX: &str = "/dev/disk";
const RAW_PREFIX: &str = "/dev/rdisk";

impl DevInfo {
    pub fn new(path: &str) -> anyhow::Result<DevInfo> {
        if path.is_empty() {
            return Err(anyhow::anyhow!("Empty device path"));
        }
        let (path, rpath) = if path.starts_with(BUF_PREFIX) {
            (path.to_owned(), path.replace(BUF_PREFIX, RAW_PREFIX))
        } else if path.starts_with(RAW_PREFIX) {
            (path.replace(RAW_PREFIX, BUF_PREFIX), path.to_owned())
        } else {
            (path.to_owned(), path.to_owned())
        };

        let mut probe = BlkidProbe::new_from_filename(Path::new(&path))
            .context("Cannot initialize device probe")?;
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

        let label = probe.lookup_value("LABEL").ok();
        let fs_type = probe.lookup_value("TYPE").ok();
        let uuid = probe.lookup_value("UUID").ok();

        Ok(DevInfo {
            path,
            rpath,
            label,
            fs_type,
            uuid,
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

    pub fn fs_type(&self) -> Option<&str> {
        self.fs_type.as_deref()
    }

    pub fn uuid(&self) -> Option<&str> {
        self.uuid.as_deref()
    }

    pub fn auto_mount_name(&self) -> &str {
        self.label()
            // .or(self.uuid())
            .unwrap_or(self.disk().split('/').last().expect("non-empty disk path"))
    }

    #[allow(unused)]
    pub fn logical_volumes(path: &str) -> Vec<Volume> {
        let mut volumes = Vec::new();
        if path.is_empty() {
            return volumes;
        }
        let path = path.to_owned();
        let mut probe = BlkidProbe::new_from_filename(Path::new(&path))
            .context("Cannot initialize device probe")
            .unwrap();
        probe
            .enable_superblocks(true)
            .context("Cannot enable device superblock probe")
            .unwrap();
        probe
            .set_superblock_flags(BlkidSublksFlags::new(vec![
                BlkidSublks::Label,
                BlkidSublks::Type,
                BlkidSublks::Uuid,
            ]))
            .context("Cannot configure device superblock probe")
            .unwrap();

        probe
            .enable_partitions(true)
            .context("Cannot enable device partition probe")
            .unwrap();

        probe
            .do_safeprobe()
            .context(format!("Cannot probe device {}", &path))
            .unwrap();

        let mut part_list = probe
            .get_partitions()
            .context("Cannot get device partitions")
            .unwrap();

        println!("Partitions: {}", part_list.number_of_partitions().unwrap());

        for i in 0..part_list.number_of_partitions().unwrap() {
            let part = part_list
                .get_partition(i)
                .context(format!("Cannot get partition {}", i))
                .unwrap();

            let vol = Volume {
                name: part.get_name().unwrap_or_default(),
                part_type: part.get_type_string().unwrap_or_default(),
                uuid: part.get_uuid().unwrap_or_default().map(|s| s.to_string()),
            };
            println!(
                "name: {:?}, type: {:?} ({}), uuid: {:?}",
                vol.name,
                vol.part_type,
                part.get_type(),
                vol.uuid,
            );
            if !part.is_logical() {
                continue;
            }
            volumes.push(vol);
        }
        volumes
    }
}

pub struct Volume {
    name: Option<String>,
    part_type: String,
    uuid: Option<String>,
}
