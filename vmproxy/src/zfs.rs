#![allow(unused)]
use anyhow::Context;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

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
pub fn mountpoints_from_json(text: &str) -> anyhow::Result<Vec<String>> {
    let parsed: ZfsList = serde_json::from_str(text).context("failed to parse zfs json")?;

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
