use std::{
    fs::{self, Permissions},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, anyhow};
use common_utils::{host_eprintln, host_println};
use glob::glob;
use serde::Serialize;

use crate::{Config, ImageSource, dnsutil, fsutil, utils};

pub fn init(config: &Config, force: bool, src: &ImageSource) -> anyhow::Result<()> {
    match src.os_type {
        // we ignore src.docker_ref for now (because only alpine:latest is supported)
        crate::OSType::Linux => init_linux_rootfs(config, force),
        crate::OSType::FreeBSD => init_freebsd_rootfs(config, force, src),
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

const BSD_ENTRYPOINT_SCRIPT_URL: &str =
    "https://raw.githubusercontent.com/nohajc/docker-nfs-server/refs/heads/freebsd/entrypoint.sh";

const FREEBSD_BOOTSTRAP_EXEC: &str = "freebsd-bootstrap";
const FREEBSD_INIT_EXEC: &str = "init-freebsd";
const FREEBSD_VMPROXY_EXEC: &str = "vmproxy-bsd";

fn init_freebsd_rootfs(config: &Config, force: bool, src: &ImageSource) -> anyhow::Result<()> {
    if !force {
        todo!() // check for existing freebsd image
    }

    // TODO: add deferred action which will call fs::remove_dir_all(tmp_path)

    if src.base_dir.is_empty() {
        return Err(anyhow!("FreeBSD base directory not specified"));
    }

    let Some(iso_image_url) = src.iso_url.as_deref() else {
        return Err(anyhow!("FreeBSD ISO URL not provided"));
    };
    let Some(oci_image_url) = src.oci_url.as_deref() else {
        return Err(anyhow!("FreeBSD OCI URL not provided"));
    };
    let Some(kernel_bundle_url) = src.kernel.bundle_url.as_deref() else {
        return Err(anyhow!("FreeBSD kernel bundle URL not provided"));
    };

    if iso_image_url.is_empty() {
        return Err(anyhow!("FreeBSD ISO URL is empty"));
    }
    if oci_image_url.is_empty() {
        return Err(anyhow!("FreeBSD OCI URL is empty"));
    }
    if kernel_bundle_url.is_empty() {
        return Err(anyhow!("FreeBSD kernel bundle URL is empty"));
    }

    let oci_image = oci_image_url
        .split('/')
        .last()
        .context("invalid FreeBSD OCI URL")?;
    let kernel_bundle = kernel_bundle_url
        .split('/')
        .last()
        .context("invalid FreeBSD kernel bundle URL")?;

    let oci_iso_image = "freebsd-oci.iso";
    let bootstrap_image = "freebsd-bootstrap.iso";
    let vm_disk_image = "freebsd-microvm-disk.img";

    let freebsd_base_path = config.profile_path.join(&src.base_dir);
    let tmp_path = freebsd_base_path.join("tmp");
    let oci_path = tmp_path.join("oci");
    fs::create_dir_all(&oci_path).context("Failed to create FreeBSD base directory")?;

    fetch(oci_image_url, &tmp_path).context("Failed to fetch FreeBSD OCI image")?;
    extract(oci_image, &tmp_path, &oci_path).context("Failed to unpack FreeBSD OCI image")?;
    create_iso(oci_iso_image, &tmp_path, &oci_path)
        .context("Failed to convert FreeBSD OCI image to ISO")?;

    let bootstrap_rootfs_path = tmp_path.join("rootfs");
    fs::create_dir_all(bootstrap_rootfs_path.join("dev"))
        .context("Failed to create rootfs/dev directory")?;

    fs::create_dir_all(bootstrap_rootfs_path.join("tmp"))
        .context("Failed to create rootfs/tmp directory")?;

    copy_file(
        config.libexec_path.join(FREEBSD_BOOTSTRAP_EXEC),
        bootstrap_rootfs_path.join(FREEBSD_BOOTSTRAP_EXEC),
    )?;
    copy_file(
        config.libexec_path.join(FREEBSD_INIT_EXEC),
        bootstrap_rootfs_path.join(FREEBSD_INIT_EXEC),
    )?;
    copy_file(
        config.libexec_path.join(FREEBSD_VMPROXY_EXEC),
        bootstrap_rootfs_path.join(FREEBSD_VMPROXY_EXEC),
    )?;

    fetch(kernel_bundle_url, &tmp_path).context("Failed to fetch FreeBSD kernel bundle")?;
    extract(kernel_bundle, &tmp_path, &freebsd_base_path)
        .context("Failed to extract FreeBSD kernel bundle")?;

    let modules = glob(
        freebsd_base_path
            .join("kernel")
            .join("*.ko")
            .to_str()
            .context("invalid FreeBSD kernel path")?,
    )
    .context("invalid glob pattern")?;

    for m in modules.into_iter().filter_map(|e| e.ok()) {
        copy_file(&m, bootstrap_rootfs_path.join(m.file_name().unwrap()))?;
    }

    fs::write(
        bootstrap_rootfs_path.join("config.json"),
        serde_json::to_string(&FreeBSDBootstrapConfig {
            iso_url: iso_image_url.into(),
            pkgs: vec!["bash".into(), "pidof".into()],
        })?,
    )
    .context("Failed to write FreeBSD bootstrap config")?;

    let entrypoint_sh = BSD_ENTRYPOINT_SCRIPT_URL.split('/').last().unwrap();
    fetch(BSD_ENTRYPOINT_SCRIPT_URL, &bootstrap_rootfs_path)
        .context("Failed to fetch FreeBSD entrypoint script")?;
    fs::set_permissions(
        bootstrap_rootfs_path.join(entrypoint_sh),
        Permissions::from_mode(0o755),
    )
    .context("Failed to set executable permissions on FreeBSD entrypoint script")?;

    create_iso(bootstrap_image, &tmp_path, &bootstrap_rootfs_path)
        .context("Failed to create FreeBSD bootstrap ISO")?;

    create_sparse_file(freebsd_base_path.join(vm_disk_image), "32G")
        .context("Failed to create FreeBSD VM disk image")?;

    // TODO:
    // 1. boot the VM to run the bootstrap process and populate our disk image
    // 2. boot it again to install third-party packages

    Ok(())
}

fn copy_file(src: impl AsRef<Path>, dest: impl AsRef<Path>) -> anyhow::Result<()> {
    fs::copy(src.as_ref(), dest.as_ref()).context(format!(
        "Failed to copy {} to {}",
        src.as_ref().display(),
        dest.as_ref().display()
    ))?;
    Ok(())
}

fn fetch(url: &str, dest_dir: &Path) -> anyhow::Result<()> {
    let curl_status = Command::new("/usr/bin/curl")
        .current_dir(dest_dir)
        .args(&["-LO"])
        .arg(url)
        .status()
        .context("Failed to execute curl command")?;

    if !curl_status.success() {
        return Err(anyhow!(
            "curl command failed with exit code {}",
            curl_status
                .code()
                .map(|c| c.to_string())
                .unwrap_or("unknown".to_owned())
        ));
    }

    Ok(())
}

const TAR: &str = "/usr/bin/bsdtar";

fn create_iso(
    iso_path: impl AsRef<Path>,
    working_dir: &Path,
    src_dir: &Path,
) -> anyhow::Result<()> {
    let tar_status = Command::new(TAR)
        .current_dir(working_dir)
        .args(&["cvf"])
        .arg(iso_path.as_ref())
        .args(&["--format", "iso9660"])
        .arg("-C")
        .arg(src_dir)
        .arg(".")
        .status()
        .context("Failed to execute tar command")?;

    if !tar_status.success() {
        return Err(anyhow!(
            "tar command failed with exit code {}",
            tar_status
                .code()
                .map(|c| c.to_string())
                .unwrap_or("unknown".to_owned())
        ));
    }

    Ok(())
}

fn extract(
    archive_path: impl AsRef<Path>,
    working_dir: &Path,
    dest_dir: &Path,
) -> anyhow::Result<()> {
    let tar_status = Command::new(TAR)
        .current_dir(working_dir)
        .args(&["xvf"])
        .arg(archive_path.as_ref())
        .arg("-C")
        .arg(dest_dir)
        .status()
        .context("Failed to execute tar command")?;

    if !tar_status.success() {
        return Err(anyhow!(
            "tar command failed with exit code {}",
            tar_status
                .code()
                .map(|c| c.to_string())
                .unwrap_or("unknown".to_owned())
        ));
    }

    Ok(())
}

fn create_sparse_file(path: impl AsRef<Path>, size_spec: &str) -> anyhow::Result<()> {
    let truncate_status = Command::new("/usr/bin/truncate")
        .arg("-s")
        .arg(size_spec)
        .arg(path.as_ref())
        .status()
        .context("Failed to execute truncate command")?;

    if !truncate_status.success() {
        return Err(anyhow!(
            "truncate command failed with exit code {}",
            truncate_status
                .code()
                .map(|c| c.to_string())
                .unwrap_or("unknown".to_owned())
        ));
    }

    Ok(())
}

#[derive(Debug, Serialize)]
struct FreeBSDBootstrapConfig {
    iso_url: String,
    pkgs: Vec<String>,
}
