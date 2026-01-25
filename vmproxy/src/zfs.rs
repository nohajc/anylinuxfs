#![allow(unused)]
use anyhow::Context;
use bstr::{BString, ByteSlice};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    io::Write,
    process::{ExitStatus, Stdio},
};

use crate::{
    utils::{script, script_output},
    zfs,
};

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct Zpool {
    pub name: String,
    pub id: String,
}

pub fn get_importable_zpools() -> anyhow::Result<Vec<Zpool>> {
    //   pool: rpool
    //     id: 12902241841912726807
    //   pool: bpool
    //     id: 16435365342370519676
    let text =
        script_output("zpool import | grep -E '(pool|id):'").context("Failed to get zpools")?;

    let mut zpool_names = HashMap::new();
    let mut zpools = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("pool: ") {
            let mut name = trimmed.strip_prefix("pool: ").unwrap().to_string();

            let cnt = zpool_names.entry(name.clone()).or_insert(0);
            if *cnt > 0 {
                name = format!("{}-{}", name, cnt);
            }
            *cnt += 1;

            zpools.push(Zpool {
                name,
                id: String::new(),
            });
        } else if trimmed.starts_with("id: ") {
            let id = trimmed.strip_prefix("id: ").unwrap().to_string();
            if let Some(last) = zpools.iter_mut().last() {
                last.id = id;
            }
        }
    }

    Ok(zpools)
}

/// Top-level structure matching the `zfs list -j` JSON output
#[derive(Debug, Deserialize)]
pub struct ZfsList {
    #[serde(rename = "output_version")]
    pub output_version: Option<OutputVersion>,
    pub datasets: HashMap<String, Dataset>,
}

#[derive(Debug, Deserialize)]
pub struct OutputVersion {
    pub command: String,
    pub vers_major: u32,
    pub vers_minor: u32,
}

#[derive(Debug, Deserialize)]
pub struct Dataset {
    pub name: String,
    #[serde(rename = "type")]
    pub ds_type: String,
    pub pool: String,
    pub createtxg: Option<String>,
    pub properties: Option<HashMap<String, Property>>,
}

#[derive(Debug, Deserialize)]
pub struct Property {
    pub value: String,
    pub source: Option<Source>,
}

#[derive(Debug, Deserialize)]
pub struct Source {
    #[serde(rename = "type")]
    pub src_type: Option<String>,
    pub data: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Mountpoint {
    pub name: String,
    pub path: String,
    pub pool: String,
    pub encrypted: bool,
}

const PROP_MOUNTPOINT: &str = "mountpoint";
const PROP_CANMOUNT: &str = "canmount";
const PROP_ENCRYPTION: &str = "encryption";
const PROPS_TO_LIST: &[&str] = &[PROP_MOUNTPOINT, PROP_CANMOUNT, PROP_ENCRYPTION];

pub fn mountpoints() -> anyhow::Result<Vec<Mountpoint>> {
    let text = script_output(&format!("zfs list -o {} -j", PROPS_TO_LIST.join(",")))
        .context("Failed to get ZFS mountpoints")?;
    mountpoints_from_json(&text)
}

const EXCLUDED_MOUNTPOINT_TYPES: &[&str] = &["-", "legacy", "none"];

fn mountpoints_from_json(text: &str) -> anyhow::Result<Vec<Mountpoint>> {
    let parsed: ZfsList = serde_json::from_str(&text).context("failed to parse zfs json")?;

    let mut out = HashSet::new();
    for (_key, ds) in parsed.datasets.into_iter() {
        if let Some(props) = ds.properties {
            if let (Some(mount_prop), Some(canmount)) =
                (props.get(PROP_MOUNTPOINT), props.get(PROP_CANMOUNT))
                && !EXCLUDED_MOUNTPOINT_TYPES.contains(&mount_prop.value.as_str())
                && canmount.value != "off"
            {
                let encrypted = props
                    .get(PROP_ENCRYPTION)
                    .map_or(false, |p| p.value != "off");
                out.insert(Mountpoint {
                    name: ds.name.clone(),
                    path: mount_prop.value.clone(),
                    pool: ds.pool.clone(),
                    encrypted,
                });
            }
        }
    }
    let mut res: Vec<_> = out.into_iter().collect();
    // sort by path lexicographically
    res.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(res)
}

pub fn import_all_zpools(
    mount_point_root: &str,
    read_only: bool,
) -> anyhow::Result<(ExitStatus, Vec<Mountpoint>, Vec<String>)> {
    let opts = if read_only { "-o readonly=on" } else { "" };
    let res = script(&format!(
        "zpool import {} -faNR {}",
        opts, &mount_point_root
    ))
    .status()
    .context("Failed to run zpool import command")?;

    if !res.success() {
        return Ok((res, Vec::new(), Vec::new()));
    }

    let zfs_mountpoints = mountpoints().context("Failed to get ZFS mountpoints after import")?;

    let mut unique_pools = vec![];
    let mut pool_set = HashSet::new();
    for mp in &zfs_mountpoints {
        if !pool_set.contains(mp.pool.as_str()) {
            unique_pools.push(mp.pool.clone());
            pool_set.insert(mp.pool.as_str());
        }
    }

    Ok((res, zfs_mountpoints, unique_pools))
}

pub fn mount_datasets(
    mountpoints: &[Mountpoint],
    env_pwds: &HashMap<usize, BString>,
) -> anyhow::Result<ExitStatus> {
    for (i, mp) in mountpoints.iter().enumerate() {
        // println!("Mounting {}", mp.name);
        let mut cmd = script(&format!("zfs mount -l {}", mp.name));

        let status = if let Some(pwd) = env_pwds.get(&(i + 1)) {
            let mut child = cmd.stdin(Stdio::piped()).spawn()?;
            {
                let mut stdin = child.stdin.take().unwrap();
                stdin.write_all(pwd.as_bytes())?;
            }
            child.wait()
        } else {
            cmd.status()
        }
        .with_context(|| format!("Failed to mount ZFS dataset {}", mp.name))?;
    }
    Ok(ExitStatus::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mountpoints_from_minimal_json() {
        let json = r#"
        {
            "output_version": { "command": "zfs list", "vers_major": 0, "vers_minor": 1 },
            "datasets": {
                "pool/ds1": {
                    "name": "pool/ds1",
                    "type": "FILESYSTEM",
                    "pool": "pool",
                    "createtxg": "1",
                    "properties": {
                        "mountpoint": { "value": "/mnt/foo", "source": { "type": "LOCAL", "data": "-" } },
                        "used": { "value": "1K", "source": { "type": "NONE", "data": "-" } }
                    }
                },
                "pool/ds2": {
                    "name": "pool/ds2",
                    "type": "FILESYSTEM",
                    "pool": "pool",
                    "createtxg": "2",
                    "properties": {
                        "mountpoint": { "value": "none", "source": { "type": "LOCAL", "data": "-" } }
                    }
                }
            }
        }
        "#;

        let mps = mountpoints_from_json(json).expect("parse should succeed");
        assert_eq!(mps.len(), 1);
        assert!(mps.contains(&Mountpoint {
            name: "pool/ds1".into(),
            path: "/mnt/foo".into(),
            pool: "pool".into(),
            encrypted: false,
        }));
    }
}
