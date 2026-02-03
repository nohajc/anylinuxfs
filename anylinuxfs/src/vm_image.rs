use crate::{Config, ImageSource, fsutil, vm_network};
use anyhow::{Context, anyhow};

use common_utils::{Deferred, host_eprintln};
use std::{fs, path::Path, process::Command};

#[cfg(feature = "freebsd")]
pub const KERNEL_IMAGE: &str = "kernel/kernel.bin";

mod alpine {
    use super::*;
    use crate::{Config, dnsutil, fsutil, utils};
    use anyhow::{Context, anyhow};
    use common_utils::{host_eprintln, host_println};
    use std::{
        fs::{self},
        os::unix::process::CommandExt,
        process::{Command, Stdio},
    };

    pub const ROOTFS_CURRENT_VERSION: &str = include_str!("../../share/alpine/rootfs.ver");

    pub fn init_rootfs(config: &Config, force: bool) -> anyhow::Result<()> {
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
                && rootfs_version_matches(&config.root_ver_file_path, ROOTFS_CURRENT_VERSION)
            {
                // host_println!("VM root filesystem is initialized");
                // rootfs is initialized but check if we need to update vmproxy executable
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

        if let Err(e) = fs::write(config.root_ver_file_path.as_path(), ROOTFS_CURRENT_VERSION) {
            host_eprintln!("Failed to write rootfs version file: {}", e);
        }

        Ok(())
    }
}

#[cfg(feature = "freebsd")]
mod freebsd {
    use super::*;
    use std::{
        fs::{self, Permissions},
        io,
        os::unix::fs::PermissionsExt,
        path::Path,
        process::Command,
        time::SystemTime,
    };

    use crate::{
        NetworkMode, VMOpts,
        devinfo::DevInfo,
        settings::{Config, ImageSource},
        setup_vm, start_vm_forked,
    };
    use anyhow::{Context, anyhow};
    use bstr::{BStr, BString};
    use common_utils::{Deferred, OSType, PathExt, host_eprintln, host_println};
    use serde::Serialize;

    pub const ENTRYPOINT_SCRIPT_URL: &str = "https://raw.githubusercontent.com/nohajc/docker-nfs-server/refs/heads/freebsd/entrypoint.sh";

    pub const BOOTSTRAP_EXEC: &str = "freebsd-bootstrap";
    pub const INIT_EXEC: &str = "init-freebsd";
    pub const VMPROXY_EXEC: &str = "vmproxy-bsd";

    pub const ROOTFS_CURRENT_VERSION: &str = include_str!("../../share/freebsd/rootfs.ver");

    pub const VM_DISK_IMAGE: &str = "freebsd-microvm-disk.img";

    pub fn init_rootfs(config: &Config, force: bool, src: &ImageSource) -> anyhow::Result<()> {
        if src.base_dir.is_empty() {
            return Err(anyhow!("FreeBSD base directory not specified"));
        }

        let base_path = config.profile_path.join(&src.base_dir);
        let mtimes_path = base_path.join("mtimes");
        let tmp_path = base_path.join("tmp");

        let init_src_path = config.libexec_path.join(INIT_EXEC);
        let vmproxy_src_path = config.libexec_path.join(VMPROXY_EXEC);
        let vm_disk_image_path = base_path.join(VM_DISK_IMAGE);

        let mut deferred = Deferred::new();
        if !force {
            let kernel_path = base_path.join(KERNEL_IMAGE);
            let vm_disk_image = base_path.join(VM_DISK_IMAGE);
            let rootfs_ver_file = base_path.join("rootfs.ver");

            let required_files_exist = kernel_path.exists() && vm_disk_image.exists();
            if required_files_exist
                && rootfs_version_matches(&rootfs_ver_file, ROOTFS_CURRENT_VERSION)
            {
                // host_println!("FreeBSD VM root filesystem is initialized");
                let mut to_upgrade = vec![];

                if get_file_mtime(&init_src_path)?
                    > read_mtime_file(&mtimes_path.join(INIT_EXEC))?
                        .unwrap_or(SystemTime::UNIX_EPOCH)
                {
                    to_upgrade.push(INIT_EXEC);
                }

                if get_file_mtime(&vmproxy_src_path)?
                    > read_mtime_file(&mtimes_path.join(VMPROXY_EXEC))?
                        .unwrap_or(SystemTime::UNIX_EPOCH)
                {
                    to_upgrade.push(VMPROXY_EXEC);
                }

                if !to_upgrade.is_empty() {
                    // prepare iso and run the upgrade script inside the VM
                    fs::create_dir_all(&tmp_path)?;
                    deferred.add(|| {
                        if let Err(e) = fs::remove_dir_all(&tmp_path) {
                            host_eprintln!("Failed to remove {}: {}", tmp_path.display(), e);
                        }
                    });

                    let upgrade_iso_image = "upgrade.iso";
                    create_iso(
                        upgrade_iso_image,
                        &tmp_path,
                        &config.libexec_path,
                        IsoAdd::Files(&to_upgrade),
                        None,
                    )?;
                    let upgrade_iso_image_path = tmp_path.join(upgrade_iso_image);

                    let devices = &[
                        DevInfo::pv(vm_disk_image_path.as_bytes())?,
                        DevInfo::pv(upgrade_iso_image_path.as_bytes())?,
                    ];
                    let cmdline = &["/upgrade-binaries.sh".into()];
                    start_freebsd_vm(&config, devices, cmdline, NetworkMode::Default)
                        .context("Failed to start FreeBSD VM for upgrade")?;

                    write_mtime_files(
                        &base_path,
                        &to_upgrade
                            .iter()
                            .map(|f| config.libexec_path.join(f))
                            .collect::<Vec<_>>(),
                    )
                    .context("Failed to write mtime files")?;

                    host_println!("Updated VM image");
                }
                return Ok(());
            }
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

        let oci_path = tmp_path.join("oci");
        fs::create_dir_all(&oci_path).context("Failed to create FreeBSD base directory")?;
        host_println!("Created FreeBSD base directory: {}", base_path.display());

        deferred.add(|| {
            if let Err(e) = fs::remove_dir_all(&tmp_path) {
                host_eprintln!("Failed to remove {}: {}", tmp_path.display(), e);
            }
        });

        fetch(oci_image_url, &tmp_path).context("Failed to fetch FreeBSD OCI image")?;
        host_println!("Fetched FreeBSD OCI image: {}", oci_image);

        extract(oci_image, &tmp_path, &oci_path).context("Failed to unpack FreeBSD OCI image")?;
        create_iso(oci_iso_image, &tmp_path, &oci_path, IsoAdd::All, None)
            .context("Failed to convert FreeBSD OCI image to ISO")?;

        let oci_iso_image_path = tmp_path.join(oci_iso_image);
        host_println!(
            "Converted FreeBSD OCI image to ISO: {}",
            oci_iso_image_path.display()
        );

        let bootstrap_rootfs_path = tmp_path.join("rootfs");
        fs::create_dir_all(bootstrap_rootfs_path.join("dev"))
            .context("Failed to create rootfs/dev directory")?;

        fs::create_dir_all(bootstrap_rootfs_path.join("mnt"))
            .context("Failed to create rootfs/mnt directory")?;

        fs::create_dir_all(bootstrap_rootfs_path.join("tmp"))
            .context("Failed to create rootfs/tmp directory")?;

        copy_file(
            config.libexec_path.join(BOOTSTRAP_EXEC),
            bootstrap_rootfs_path.join(BOOTSTRAP_EXEC),
        )?;
        host_println!(
            "Copied {} to {}",
            BOOTSTRAP_EXEC,
            bootstrap_rootfs_path.display()
        );

        copy_file(&init_src_path, bootstrap_rootfs_path.join(INIT_EXEC))?;
        host_println!(
            "Copied {} to {}",
            INIT_EXEC,
            bootstrap_rootfs_path.display()
        );

        copy_file(&vmproxy_src_path, bootstrap_rootfs_path.join(VMPROXY_EXEC))?;
        host_println!(
            "Copied {} to {}",
            VMPROXY_EXEC,
            bootstrap_rootfs_path.display()
        );

        fetch(kernel_bundle_url, &tmp_path).context("Failed to fetch FreeBSD kernel bundle")?;
        host_println!("Fetched FreeBSD kernel bundle: {}", kernel_bundle_url);

        extract(kernel_bundle, &tmp_path, &base_path)
            .context("Failed to extract FreeBSD kernel bundle")?;

        let modules = fs::read_dir(base_path.join("kernel"))?
            .filter_map(|e| e.ok())
            .filter_map(|e| match e.path().extension() {
                Some(ext) if ext == "ko" => Some(e.path()),
                _ => None,
            });

        for m in modules {
            copy_file(&m, bootstrap_rootfs_path.join(m.file_name().unwrap()))?;
            host_println!(
                "Copied {} to {}",
                m.display(),
                bootstrap_rootfs_path.display()
            );
        }

        fs::write(
            bootstrap_rootfs_path.join("config.json"),
            serde_json::to_string(&FreeBSDBootstrapConfig {
                iso_url: iso_image_url.into(),
                pkgs: vec!["bash".into(), "pidof".into()],
            })?,
        )
        .context("Failed to write FreeBSD bootstrap config")?;

        host_println!(
            "Prepared FreeBSD bootstrap config: {}",
            bootstrap_rootfs_path.join("config.json").display()
        );

        let entrypoint_sh = ENTRYPOINT_SCRIPT_URL.split('/').last().unwrap();
        fetch(ENTRYPOINT_SCRIPT_URL, &bootstrap_rootfs_path)
            .context("Failed to fetch FreeBSD entrypoint script")?;
        host_println!(
            "Fetched FreeBSD entrypoint script: {}",
            ENTRYPOINT_SCRIPT_URL
        );
        fs::set_permissions(
            bootstrap_rootfs_path.join(entrypoint_sh),
            Permissions::from_mode(0o755),
        )
        .context("Failed to set executable permissions on FreeBSD entrypoint script")?;

        create_iso(
            bootstrap_image,
            &tmp_path,
            &bootstrap_rootfs_path,
            IsoAdd::All,
            None,
        )
        .context("Failed to create FreeBSD bootstrap ISO")?;

        let bootstrap_image_path = tmp_path.join(bootstrap_image);
        host_println!(
            "Created FreeBSD bootstrap ISO: {}",
            bootstrap_image_path.display()
        );

        _ = fs::remove_file(&vm_disk_image_path);
        create_sparse_file(&vm_disk_image_path, "32G")
            .context("Failed to create FreeBSD VM disk image")?;

        host_println!(
            "Created FreeBSD VM disk image: {}",
            vm_disk_image_path.display()
        );

        // 1. boot the VM to run the bootstrap process and populate our disk image
        let bstrap_status = setup_gvproxy(&config, || {
            start_freebsd_bootstrap_vm(
                &config,
                bootstrap_image_path.as_bytes(),
                oci_iso_image_path.as_bytes(),
                vm_disk_image_path.as_bytes(),
            )
        })?;
        if bstrap_status != 0 {
            return Err(anyhow!(
                "FreeBSD bootstrap VM exited with status {}",
                bstrap_status
            ));
        }

        // 2. boot it again to install third-party packages
        let setup_status = setup_gvproxy(&config, || {
            let devices = &[DevInfo::pv(vm_disk_image_path.as_bytes())?];
            let cmdline = &["/usr/local/bin/vm-setup.sh".into()];
            start_freebsd_vm(
                &config,
                devices,
                cmdline,
                NetworkMode::default_for_os(OSType::FreeBSD),
            )
        })?;
        if setup_status != 0 {
            return Err(anyhow!(
                "FreeBSD VM setup exited with status {}",
                setup_status
            ));
        }

        // 3. write rootfs version file
        let root_ver_file_path = base_path.join("rootfs.ver");
        if let Err(e) = fs::write(root_ver_file_path, ROOTFS_CURRENT_VERSION) {
            host_eprintln!("Failed to write rootfs version file: {}", e);
        }

        write_mtime_files(&base_path, &[&init_src_path, &vmproxy_src_path])
            .context("Failed to write mtime files")?;

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

    fn extract(
        archive_path: impl AsRef<Path>,
        working_dir: &Path,
        dest_dir: &Path,
    ) -> anyhow::Result<()> {
        let tar_status = Command::new(TAR)
            .current_dir(working_dir)
            .args(&["xf"])
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

    fn start_freebsd_bootstrap_vm(
        config: &Config,
        bootstrap_image_path: impl AsRef<BStr>,
        oci_iso_image_path: impl AsRef<BStr>,
        vm_disk_image_path: impl AsRef<BStr>,
    ) -> anyhow::Result<i32> {
        let devices = &[
            DevInfo::pv(bootstrap_image_path)?,
            DevInfo::pv(vm_disk_image_path)?,
            DevInfo::pv(oci_iso_image_path)?,
        ];

        let opts = VMOpts::new()
            .root_device("cd9660:/dev/vtbd0")
            .legacy_console(true);
        let ctx = setup_vm(
            &config,
            devices,
            NetworkMode::default_for_os(OSType::FreeBSD),
            false,
            opts,
        )?;
        let bstrap_status = start_vm_forked(&ctx, &["/freebsd-bootstrap".into()], &[])
            .context("Failed to start FreeBSD bootstrap VM")?;

        if bstrap_status != 0 {
            return Err(anyhow::anyhow!(
                "bootstrap microVM exited with status {}",
                bstrap_status
            ));
        }
        Ok(bstrap_status)
    }

    fn start_freebsd_vm(
        config: &Config,
        devices: &[DevInfo],
        cmdline: &[BString],
        net_mode: NetworkMode,
    ) -> anyhow::Result<i32> {
        let opts = VMOpts::new()
            .root_device("ufs:/dev/gpt/rootfs")
            .legacy_console(true);
        let ctx = setup_vm(&config, devices, net_mode, false, opts)?;
        let setup_status =
            start_vm_forked(&ctx, cmdline, &[]).context("Failed to start FreeBSD VM setup")?;

        if setup_status != 0 {
            return Err(anyhow::anyhow!(
                "FreeBSD VM setup exited with status {}",
                setup_status
            ));
        }
        Ok(setup_status)
    }

    fn read_mtime_file(path: &Path) -> anyhow::Result<Option<SystemTime>> {
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err).context("Failed to read mtime file"),
        };
        let mtime: SystemTime =
            ron::de::from_str(&content).context("Failed to parse mtime file")?;
        Ok(Some(mtime))
    }

    fn get_file_mtime(path: &Path) -> anyhow::Result<SystemTime> {
        let metadata = fs::metadata(path).context("Failed to get file metadata")?;
        let mtime = metadata
            .modified()
            .context("Failed to get file modified time")?;
        Ok(mtime)
    }

    fn write_mtime_files(base_path: &Path, files: &[impl AsRef<Path>]) -> anyhow::Result<()> {
        let mtimes_path = base_path.join("mtimes");
        fs::create_dir_all(&mtimes_path).context("Failed to create mtimes directory")?;

        for f in files {
            let target_path = mtimes_path.join(f.as_ref().file_name().unwrap());
            let mtime = fs::metadata(f.as_ref())?.modified()?;
            fs::write(target_path, ron::ser::to_string(&mtime)?)
                .context("Failed to write mtime file")?;
        }
        Ok(())
    }
}

#[allow(unused)]
pub enum IsoAdd<'a, 'b> {
    All,
    Files(&'a [&'b str]),
}

const TAR: &str = "/usr/bin/bsdtar";

pub fn create_iso(
    iso_path: impl AsRef<Path>,
    working_dir: impl AsRef<Path>,
    src_dir: impl AsRef<Path>,
    file_list: IsoAdd<'_, '_>,
    label: Option<&str>,
) -> anyhow::Result<()> {
    let tar_status = Command::new(TAR)
        .current_dir(working_dir)
        .args(&["cf"])
        .arg(iso_path.as_ref())
        .args(&["--format", "iso9660"])
        .args(
            label
                .into_iter()
                .flat_map(|l| vec!["--options".into(), format!("volume-id={}", l)]),
        )
        .arg("-C")
        .arg(src_dir.as_ref())
        .args(match file_list {
            IsoAdd::All => &["."],
            IsoAdd::Files(files) => files,
        })
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

pub fn init(config: &Config, force: bool, src: &ImageSource) -> anyhow::Result<()> {
    match src.os_type {
        // we ignore src.docker_ref for now (because only alpine:latest is supported)
        crate::OSType::Linux => alpine::init_rootfs(config, force),
        #[cfg(feature = "freebsd")]
        crate::OSType::FreeBSD => freebsd::init_rootfs(config, force, src),
        #[cfg(not(feature = "freebsd"))]
        _ => Err(anyhow::anyhow!("unsupported OS type")),
    }
}

#[cfg(feature = "freebsd")]
pub fn remove(config: &Config, src: &ImageSource) -> anyhow::Result<()> {
    let base_path = config.profile_path.join(&src.base_dir);
    fs::remove_dir_all(&base_path)?;
    Ok(())
}

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

#[cfg(feature = "vmnet")]
pub fn setup_vmnet_helper(
    config: &Config,
    start_vm_fn: impl FnOnce() -> anyhow::Result<i32>,
) -> anyhow::Result<i32> {
    let mut deferred = Deferred::new();

    let (mut vmnet_helper, _vmnet_config) = vm_network::start_vmnet_helper(&config)?;
    // host_println!("vmnet-helper started with config: {:?}", _vmnet_config);
    fsutil::wait_for_file(&config.unixgram_sock_path)?;

    _ = deferred.add(|| {
        if let Err(e) = vm_network::vfkit_sock_cleanup(&config.unixgram_sock_path) {
            host_eprintln!("{:#}", e);
        }
    });

    if let Some(status) = vmnet_helper.try_wait().ok().flatten() {
        return Err(anyhow!(
            "vmnet-helper failed with exit code: {}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or("unknown".to_owned())
        ));
    }

    _ = deferred.add(move || {
        if let Err(e) = common_utils::terminate_child(&mut vmnet_helper, "vmnet-helper", None) {
            host_eprintln!("{:#}", e);
        }
    });

    start_vm_fn()
}

pub fn setup_gvproxy(
    config: &Config,
    start_vm_fn: impl FnOnce() -> anyhow::Result<i32>,
) -> anyhow::Result<i32> {
    let mut deferred = Deferred::new();

    let mut gvproxy = vm_network::start_gvproxy(&config)?;
    fsutil::wait_for_file(&config.unixgram_sock_path)?;

    _ = deferred.add(|| {
        if let Err(e) = vm_network::vfkit_sock_cleanup(&config.unixgram_sock_path) {
            host_eprintln!("{:#}", e);
        }
    });

    if let Some(status) = gvproxy.try_wait().ok().flatten() {
        return Err(anyhow!(
            "gvproxy failed with exit code: {}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or("unknown".to_owned())
        ));
    }

    _ = deferred.add(move || {
        if let Err(e) = common_utils::terminate_child(&mut gvproxy, "gvproxy", None) {
            host_eprintln!("{:#}", e);
        }
    });

    start_vm_fn()
}
