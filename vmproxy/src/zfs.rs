#![allow(unused)]
use anyhow::Context;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

use crate::utils::script_output;

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct Zpool {
    name: String,
    id: String,
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
    pub pool: Option<String>,
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

/// Parse JSON text containing the ZFS output and return the mountpoint values for every
/// dataset that defines a `mountpoint` property.
pub fn mountpoints() -> anyhow::Result<Vec<String>> {
    let text = script_output("zfs list -j").context("Failed to get ZFS mountpoints")?;
    mountpoints_from_json(&text)
}

fn mountpoints_from_json(text: &str) -> anyhow::Result<Vec<String>> {
    let parsed: ZfsList = serde_json::from_str(&text).context("failed to parse zfs json")?;

    let mut out = HashSet::new();
    for (_key, ds) in parsed.datasets.into_iter() {
        if let Some(props) = ds.properties {
            if let Some(mount_prop) = props.get("mountpoint")
                && mount_prop.value != "none"
            {
                out.insert(mount_prop.value.clone());
            }
        }
    }
    Ok(out.into_iter().collect())
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
        assert_eq!(mps.len(), 2);
        assert!(mps.contains(&"/mnt/foo".to_string()));
        assert!(mps.contains(&"none".to_string()));
    }
}
