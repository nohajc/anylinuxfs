use std::{
    fs,
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, anyhow};
use common_utils::{host_eprintln, host_println};

use crate::{Config, ImageSource, dnsutil, fsutil, utils};

pub fn init(config: &Config, force: bool, src: &ImageSource) -> anyhow::Result<()> {
    match src.os_type {
        // we ignore src.docker_ref for now (because only alpine:latest is supported)
        crate::OSType::Linux => init_linux_rootfs(config, force),
        crate::OSType::FreeBSD => todo!(),
    }
}

fn init_linux_rootfs(config: &Config, force: bool) -> anyhow::Result<()> {
    if !force {
        let bash_path = config.root_path.join("bin/bash");
        let nfsd_path = config.root_path.join("usr/sbin/rpc.nfsd");
        let entry_point_path = config.root_path.join("usr/local/bin/entrypoint.sh");
        let vmproxy_guest_path = config.root_path.join("vmproxy");
        let required_files_exist = bash_path.exists()
            && nfsd_path.exists()
            && entry_point_path.exists()
            && vmproxy_guest_path.exists();

        let fstab_path = config.root_path.join("etc/fstab");

        // check if fstab contains rpc_pipefs and nfsd keywords
        let fstab_configured = match fstab_path.exists() {
            true => {
                let fstab_content = std::fs::read_to_string(&fstab_path).context(format!(
                    "Failed to read fstab file: {}",
                    fstab_path.display()
                ))?;
                fstab_content.contains("rpc_pipefs") && fstab_content.contains("nfsd")
            }
            false => false,
        };
        if required_files_exist
            && fstab_configured
            && rootfs_version_matches(&config.root_ver_file_path, ROOTFS_ALPINE_CURRENT_VERSION)
        {
            // host_println!("VM root filesystem is initialized");
            // rootfs should be initialized but check if we need to update vmproxy executable
            if fsutil::files_likely_differ(&config.vmproxy_host_path, &vmproxy_guest_path)? {
                fs::copy(&config.vmproxy_host_path, &vmproxy_guest_path).context(format!(
                    "Failed to copy {} to {}",
                    config.vmproxy_host_path.display(),
                    vmproxy_guest_path.display()
                ))?;
                host_println!("Updated VM root filesystem");
            }
            return Ok(());
        }
    }

    host_println!("Initializing VM root filesystem...");

    let mut init_rootfs_cmd = Command::new(&config.init_rootfs_path);
    if let (Some(uid), Some(gid)) = (config.sudo_uid, config.sudo_gid) {
        // run init-rootfs with dropped privileges
        init_rootfs_cmd.uid(uid).gid(gid);
    }

    let dns_server = dnsutil::get_dns_server_with_fallback();

    let mut hnd = init_rootfs_cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args(&["-n", &dns_server])
        .spawn()
        .context("Failed to execute init-rootfs")?;

    utils::echo_child_output(&mut hnd, None);
    let status = hnd.wait().context("Failed to wait for init-rootfs")?;

    if !status.success() {
        return Err(anyhow!(
            "init-rootfs failed with exit code {}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or("unknown".to_owned())
        ));
    }

    if let Err(e) = fs::write(
        config.root_ver_file_path.as_path(),
        ROOTFS_ALPINE_CURRENT_VERSION,
    ) {
        host_eprintln!("Failed to write rootfs version file: {}", e);
    }

    Ok(())
}

const ROOTFS_ALPINE_CURRENT_VERSION: &str = "1.2.0";

fn rootfs_version_matches(root_ver_file_path: &Path, current_version: &str) -> bool {
    let version = if root_ver_file_path.exists() {
        fs::read_to_string(root_ver_file_path)
            .unwrap_or_default()
            .trim()
            .to_string()
    } else {
        "".into()
    };
    if version != current_version {
        host_eprintln!("New version detected.");
        return false;
    }
    true
}
