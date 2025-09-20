#![allow(unused)]
use anyhow::Context;
use std::collections::HashMap;

#[cfg(target_os = "linux")]
pub fn kernel_config() -> anyhow::Result<HashMap<String, String>> {
    // Use procfs only on Linux
    let mut out: HashMap<String, String> = HashMap::new();
    let cfg = procfs::kernel_config().context("failed to read kernel config via procfs")?;
    for (k, v) in cfg {
        // Convert the procfs ConfigSetting (or similar) to a String representation.
        out.insert(k, format!("{:?}", v));
    }
    Ok(out)
}

#[cfg(not(target_os = "linux"))]
pub fn kernel_config() -> anyhow::Result<HashMap<String, String>> {
    // On non-Linux hosts, return an empty map instead of compiling procfs.
    Ok(HashMap::new())
}
