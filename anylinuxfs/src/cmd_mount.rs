use anyhow::{Context, anyhow};
use bstr::{BString, ByteSlice, ByteVec};
use common_utils::{
    Deferred, NetHelper, OSType, PathExt, host_eprintln, host_println, ipc, log, safe_println,
    vmctrl,
};

use dns_sd::{DNSRecord, DNSService};

use std::borrow::Cow;
use std::collections::{BTreeSet, HashSet};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::chown;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime};
use std::{env, iter, thread};
use std::{
    io::{self, BufRead, BufReader, Write},
    net::{IpAddr, Ipv4Addr, TcpStream, ToSocketAddrs},
};

use crate::devinfo::DevInfo;
use crate::netutil::Host;
use crate::settings::{
    Config, CustomActionEnvironment, ImageSource, KernelPage, MountConfig, PassphrasePromptConfig,
    Preferences,
};
use crate::utils::{
    self, AcquireLock, CommFd, FlockKind, HasCommFd, HasPtyFd, LockFile, OutputAction,
    PassthroughBufReader, StatusError, write_to_pipe,
};
use crate::vm::*;
use crate::{
    ConsoleLogGuard, LOCK_FILE, api, cli::*, diskutil, drop_effective_privileges, drop_privileges,
    elevate_effective_privileges, fsutil, load_mount_config, netutil, parse_vm_tag_value,
    rand_string, rpcbind, to_exit_code, vm_image, vm_network,
};

pub(crate) enum NfsStatus {
    Ready(NfsReadyState),
    Failed(Option<i32>),
}

#[derive(Debug)]
pub(crate) struct NfsReadyState {
    fslabel: Option<String>,
    fstype: Option<String>,
    changed_to_ro: bool,
    exports: Vec<String>,
}

impl NfsStatus {
    fn ok(&self) -> bool {
        matches!(self, NfsStatus::Ready(_))
    }
}

fn wait_for_nfs_server(
    vm_host: &str,
    port: u16,
    vm_dns_rec: &mut Option<DNSRecord>,
    nfs_notify_rx: mpsc::Receiver<NfsStatus>,
) -> anyhow::Result<NfsStatus> {
    // this will block until NFS server is ready or the VM exits
    let nfs_ready = nfs_notify_rx.recv()?;

    if nfs_ready.ok() {
        // make sure DNS record is already set (if applicable)
        if let Some(rec) = vm_dns_rec.as_mut() {
            rec.wait_for_registration()
                .context("Could not set DNS record for the VM")?;
        }
        // also check if the port is open
        let addr = (vm_host, port)
            .to_socket_addrs()?
            .next()
            .context("Failed to resolve VM host address")?;
        host_println!("Checking NFS server on {:?}...", addr);

        match TcpStream::connect_timeout(&addr, Duration::from_secs(10)) {
            Ok(_) => {
                return Ok(nfs_ready);
            }
            Err(e) => {
                host_eprintln!("Error connecting to port {}: {}", port, e);
                return Ok(NfsStatus::Failed(None));
            }
        }
    }

    Ok(nfs_ready)
}

pub(crate) fn unmount_fs(volume_path: &Path) -> anyhow::Result<()> {
    let status = Command::new("diskutil")
        .arg("unmount")
        .arg(volume_path)
        .status()?;

    if !status.success() {
        return Err(anyhow!(
            "umount failed with exit code {}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or("unknown".to_owned())
        ));
    }
    Ok(())
}

pub(crate) fn send_quit_cmd(config: &Config, vm_native_ip: Option<Ipv4Addr>) -> anyhow::Result<()> {
    let mut stream =
        vm_network::connect_to_vm_ctrl_socket(config, vm_native_ip, Some(Duration::from_secs(5)))?;

    ipc::Client::write_request(&mut stream, &vmctrl::Request::Quit)?;
    stream.flush()?;

    // we don't care about the response contents
    let _: vmctrl::Response = ipc::Client::read_response(&mut stream)?;

    Ok(())
}

fn terminate_child(child: &mut Child, child_name: &str) -> anyhow::Result<()> {
    common_utils::terminate_child(child, child_name, Some(log::Prefix::Host))
}

fn wait_for_vm_status(pid: libc::pid_t) -> anyhow::Result<Option<i32>> {
    let mut status = 0;
    let wait_result = unsafe { libc::waitpid(pid, &mut status, 0) };
    let last_error = io::Error::last_os_error();
    if wait_result < 0 {
        if last_error.raw_os_error().unwrap() == libc::ECHILD {
            return Ok(None);
        }
        host_eprintln!("Failed to wait for child process: {}", last_error);
        return Err(last_error.into());
    }
    host_println!("libkrun VM exited with status: {}", to_exit_code(status));
    Ok(Some(status))
}

// when the process isn't a child
pub(crate) fn wait_for_proc_exit(pid: libc::pid_t) -> anyhow::Result<()> {
    wait_for_proc_exit_with_timeout(pid, Duration::from_secs(5))
}

fn wait_for_proc_exit_with_timeout(pid: libc::pid_t, timeout: Duration) -> anyhow::Result<()> {
    let start = Instant::now();
    loop {
        if start.elapsed() > timeout {
            return Err(anyhow!("Timeout waiting for process exit"));
        }
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let buf_len = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let ret = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                buf_len,
            )
        };
        if ret != buf_len {
            let last_error = io::Error::last_os_error();
            if last_error.raw_os_error().unwrap() == libc::ESRCH {
                // process exited
                break;
            }
            return Err(last_error).context("failed to get process info");
        }
        // println!("pbi_status: {}", info.pbi_status);
        if info.pbi_status == libc::SZOMB {
            // process became a zombie
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

pub(crate) enum MountStatus<'a> {
    NotYet,
    Mounted(&'a Path),
    NoLonger,
}

pub(crate) fn validated_mount_point(rt_info: &api::RuntimeInfo) -> MountStatus<'_> {
    let Some(mount_point) = rt_info.mount_point.as_ref().map(Path::new) else {
        return MountStatus::NotYet;
    };

    let expected_mount_point = match rt_info.mount_config.get_action() {
        Some(action) if !action.override_nfs_export().is_empty() => {
            BString::from(action.override_nfs_export())
        }

        _ => {
            let share_name = match rt_info.mount_config.custom_mount_name() {
                Some(name) => name.as_bytes().into(),
                None => rt_info.dev_info.auto_mount_name(),
            };
            [b"/mnt/", share_name.as_slice()].concat().into()
        }
    };
    let expected_mount_dev = [
        rt_info.vm_host.as_slice(),
        b":",
        expected_mount_point.as_slice(),
    ]
    .concat();
    match fsutil::mounted_from(&mount_point) {
        Ok(mount_dev) if mount_dev == Path::from_bytes(&expected_mount_dev) => {
            MountStatus::Mounted(mount_point)
        }
        _ => MountStatus::NoLonger,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DevType {
    Direct,
    PV,
    LV,
}

fn print_dev_info(dev_info: &DevInfo, dev_type: DevType) {
    if dev_type == DevType::Direct || dev_type == DevType::PV {
        host_println!("disk: {}", dev_info.disk().display());
        host_println!("rdisk: {}", dev_info.rdisk().display());
        host_println!("media_writable: {}", dev_info.media_writable());
    }

    if dev_type == DevType::Direct || dev_type == DevType::LV {
        if let Some(block_size) = dev_info.block_size() {
            host_println!("block size: {}", block_size);
        }
        host_println!("label: {:?}", dev_info.label());
        host_println!("fs_type: {:?}", dev_info.fs_type());
        host_println!("uuid: {:?}", dev_info.uuid());
        host_println!("mount name: {}", dev_info.auto_mount_name());
    }
}

/// Parse a token of the form `<image_path>@s<N>` where <N> is 1-based partition number.
/// Returns Some((image_path, N)) if the token matches the pattern (checks format only).
fn parse_image_partition_ident(s: &str) -> Option<(&str, usize)> {
    if let Some(at_pos) = s.rfind("@s") {
        let (image_path, suffix) = s.split_at(at_pos);
        if image_path.is_empty() {
            return None;
        }
        let suffix = &suffix[2..];
        if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(n) = suffix.parse::<usize>() {
                return Some((image_path, n));
            }
        }
    }
    None
}

/// Resolve a disk token (block device name, path, or image@sN) into a (DevInfo, File) pair.
/// Does NOT check is_mounted — that is the caller's responsibility.
fn resolve_disk_token(token: &str, read_only: bool) -> anyhow::Result<(DevInfo, File)> {
    // Try image partition syntax first: image@sN
    if let Some((image_path, part_num)) = parse_image_partition_ident(token) {
        if !Path::new(image_path).exists() {
            return Err(anyhow!("Image file not found: {}", image_path));
        }
        let probe_devs = DevInfo::probe_image(BString::from(image_path.as_bytes()))
            .context("Failed to probe image")?;
        if part_num == 0 || part_num >= probe_devs.len() {
            return Err(anyhow!(
                "Partition {} out of range (image has {} partitions)",
                part_num,
                probe_devs.len() - 1
            ));
        }
        let partition_info = probe_devs[part_num].clone();
        let disk = File::open(partition_info.rdisk())
            .context("Failed to open image file")?
            .acquire_lock(if read_only {
                FlockKind::Shared
            } else {
                FlockKind::Exclusive
            })
            .context("Failed to acquire lock on image file")?;
        print_dev_info(&partition_info, DevType::Direct);
        return Ok((partition_info, disk));
    }

    // Check if token is an explicit file path (not a block device shorthand)
    let token_path = Path::new(token);
    if token_path.is_file() {
        // Whole image file (no partition spec)
        let dev_info = DevInfo::pv(token, true)?;
        let disk = File::open(dev_info.rdisk())
            .context("Failed to open image file")?
            .acquire_lock(if read_only {
                FlockKind::Shared
            } else {
                FlockKind::Exclusive
            })
            .context("Failed to acquire lock on image file")?;
        print_dev_info(&dev_info, DevType::Direct);
        return Ok((dev_info, disk));
    }

    // Block device: disk7s1 or /dev/disk7s1
    let dev_path_str = if token.starts_with("/dev/") {
        token.to_string()
    } else {
        format!("/dev/{}", token)
    };

    let dev_info = DevInfo::pv(dev_path_str.as_bytes().as_bstr(), false)?;
    let disk = File::open(dev_info.rdisk())
        .context("Failed to open device")?
        .acquire_lock(if read_only {
            FlockKind::Shared
        } else {
            FlockKind::Exclusive
        })
        .context("Failed to acquire lock on device")?;
    print_dev_info(&dev_info, DevType::Direct);
    Ok((dev_info, disk))
}

pub(crate) fn claim_devices(
    config: &mut MountConfig,
) -> anyhow::Result<(Vec<DevInfo>, DevInfo, Vec<File>)> {
    let mount_table = fsutil::MountTable::new()?;
    // host_println!("Current mount table: {:#?}", mount_table);

    let mut dev_infos = Vec::new();
    let mut disks = Vec::new();

    let disk_path = config.disk_path.as_str();

    let mut mnt_dev_info = if disk_path.starts_with("lvm:") {
        // example: lvm:vg1:disk7s1:lvol0 or lvm:vg1:disk7s1:image.img@s1:lvol0
        let disk_ident: Vec<&str> = disk_path.split(':').collect();
        if disk_ident.len() < 4 {
            return Err(anyhow!("Invalid LVM disk path"));
        }

        let vm_path = format!(
            "/dev/mapper/{}-{}",
            disk_ident[1].replace("-", "--"),
            disk_ident[disk_ident.len() - 1].replace("-", "--")
        );

        for (i, &di) in disk_ident.iter().skip(2).enumerate() {
            if i == disk_ident.len() - 3 {
                break;
            }
            let (dev_info, disk) = resolve_disk_token(di, config.read_only)?;
            if !dev_info.is_image() && mount_table.is_mounted(dev_info.disk()) {
                return Err(anyhow!("{} is already mounted", dev_info.disk().display()));
            }

            if dev_info.fs_type() == Some("linux_raid_member") {
                config.assemble_raid = true;
            }

            dev_infos.push(dev_info);
            disks.push(disk);
        }

        // fs label will be obtained later from the VM output
        let lv_info = DevInfo::lv(disk_path, None, vm_path)?;
        print_dev_info(&lv_info, DevType::LV);
        lv_info
    } else if disk_path.starts_with("raid:") {
        // example: raid:disk7s1:disk8s1 or raid:disk7s1:image.img@s1
        let disk_ident: Vec<&str> = disk_path.split(':').collect();
        if disk_ident.len() < 2 {
            return Err(anyhow!("Invalid RAID disk path"));
        }

        let vm_path = "/dev/md127";
        config.assemble_raid = true;

        for (_, &di) in disk_ident.iter().skip(1).enumerate() {
            let (dev_info, disk) = resolve_disk_token(di, config.read_only)?;
            if !dev_info.is_image() && mount_table.is_mounted(dev_info.disk()) {
                return Err(anyhow!("{} is already mounted", dev_info.disk().display()));
            }

            dev_infos.push(dev_info);
            disks.push(disk);
        }

        // fs label will be obtained later from the VM output
        let lv_info = DevInfo::lv(disk_path, None, vm_path)?;
        print_dev_info(&lv_info, DevType::LV);
        lv_info
    } else if disk_path.is_empty() {
        // diskless mode
        DevInfo::default()
    } else {
        // Multi-disk (colon-separated): disk1:disk2 or img1.img@s1:img2.img@s2 or mixed
        let disk_paths: Vec<_> = disk_path.split(":").collect();

        for token in disk_paths {
            // Try to resolve as image partition or image file first, then as block device
            if parse_image_partition_ident(token).is_some() {
                let (dev_info, disk) = resolve_disk_token(token, config.read_only)?;
                dev_infos.push(dev_info);
                disks.push(disk);
            } else if Path::new(token).is_file() {
                // Image file
                let (dev_info, disk) = resolve_disk_token(token, config.read_only)?;
                dev_infos.push(dev_info);
                disks.push(disk);
            } else {
                // Block device path or shorthand (disk7s1 -> /dev/disk7s1)
                let dev_path = if token.starts_with("/dev/") {
                    token.to_owned()
                } else {
                    format!("/dev/{}", token)
                };
                if !Path::new(&dev_path).exists() {
                    return Err(anyhow!("disk {} not found", dev_path));
                }
                if mount_table.is_mounted(&dev_path) {
                    if config.allow_remount {
                        unmount_fs(Path::new(&dev_path))?;
                        println!("Remounting with anylinuxfs...");
                    } else {
                        return Err(anyhow!("{} is already mounted", dev_path));
                    }
                }
                let (dev_info, disk) = resolve_disk_token(token, config.read_only)?;
                dev_infos.push(dev_info);
                disks.push(disk);
            }
        }

        dev_infos[0].clone()
    };

    if let Some(fs_driver) = &config.fs_driver {
        mnt_dev_info.set_fs_driver(&fs_driver);
    };

    if config.kernel_page_size == Some(KernelPage::Size4K)
        || (mnt_dev_info.fs_type() == Some("f2fs") && mnt_dev_info.block_size() == Some(4096))
    {
        host_println!("using 4K-page kernel");
        config.common.kernel.path = config.common.kernel.path.parent().unwrap().join("Image-4K");
    }

    if !mnt_dev_info.media_writable() && !config.read_only {
        config.read_only = true;
    }

    Ok((dev_infos, mnt_dev_info, disks))
}

pub(crate) fn ensure_enough_ram_for_luks(config: &mut Config) {
    if config.preferences.krun_ram_size_mib() < 2560 {
        config.preferences.user_mut().krun.ram_size_mib = Some(2560);
        println!(
            "Configured RAM size is lower than the minimum required for LUKS decryption, setting to {} MiB",
            config.preferences.krun_ram_size_mib()
        );
    }
}

const DEFAULT_PACKAGES_DATA: &str = include_str!("../../init-rootfs/default-alpine-packages.txt");

pub(crate) fn get_default_packages() -> BTreeSet<String> {
    DEFAULT_PACKAGES_DATA
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect()
}

pub(crate) fn prepare_vm_environment(config: &MountConfig) -> anyhow::Result<(Vec<BString>, bool)> {
    let mut env_vars = Vec::new();
    let mut env_has_passphrase = false;
    for (name, value) in env::vars_os() {
        let mut var_str = BString::from(name.as_bytes());
        if var_str.starts_with(b"ALFS_PASSPHRASE") {
            var_str.push_str(b"=");
            var_str.push_str(value.as_bytes());
            env_vars.push(var_str);
            env_has_passphrase = true;
        }
    }
    if let Some(action) = config.get_action() {
        action.prepare_environment(&mut env_vars)?;
    }
    Ok((env_vars, env_has_passphrase))
}

#[derive(Debug, Default)]
pub(crate) struct NetworkEnv {
    pub(crate) rpcbind_running: bool,
    pub(crate) usable_loopback_ip: Option<Host>,
    pub(crate) active_vm_hosts: HashSet<String>,
}

pub(crate) fn discover_api_sockets() -> anyhow::Result<Vec<PathBuf>> {
    let mut sockets = Vec::new();

    if let Ok(entries) = fs::read_dir("/tmp") {
        for entry in entries.flatten() {
            if let Some(filename) = entry.file_name().to_str() {
                if filename.starts_with("anylinuxfs") && filename.ends_with(".sock") {
                    sockets.push(entry.path());
                }
            }
        }
    }

    Ok(sockets)
}

pub(crate) fn get_runtime_info_from_socket(socket_path: &Path) -> anyhow::Result<api::RuntimeInfo> {
    api::UnixClient::make_request(socket_path, api::Request::GetConfig).and_then(
        |resp| match resp {
            api::Response::Config(rt_info) => Ok(rt_info),
        },
    )
}

pub(crate) fn collect_active_instances() -> (Vec<api::RuntimeInfo>, Vec<PathBuf>) {
    let mut instances = Vec::new();
    let mut stale_sockets = Vec::new();

    if let Ok(sockets) = discover_api_sockets() {
        for socket in sockets {
            match get_runtime_info_from_socket(&socket) {
                Ok(rt_info) => instances.push(rt_info),
                Err(_) => {
                    stale_sockets.push(socket);
                }
            }
        }
    }
    (instances, stale_sockets)
}

const LOG_RETENTION_COUNT: usize = 10;

/// Collect files with their mtimes that match a given predicate
pub(crate) fn collect_files_with_mtime<F>(dir: &Path, predicate: F) -> Vec<(PathBuf, SystemTime)>
where
    F: Fn(&str) -> bool,
{
    let mut log_files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Ok(metadata) = entry.metadata() {
                let path = entry.path();
                if let Some(filename) = path.file_name() {
                    if let Some(fname_str) = filename.to_str() {
                        if predicate(fname_str) {
                            if let Ok(modified) = metadata.modified() {
                                log_files.push((path, modified));
                            }
                        }
                    }
                }
            }
        }
    }
    log_files
}

pub(crate) fn cleanup_old_logs<'a>(
    log_dir: &Path,
    active_instances: impl IntoIterator<Item = &'a api::RuntimeInfo>,
) -> anyhow::Result<()> {
    // Collect active log paths from pre-discovered instances
    let mut active_log_paths = HashSet::new();
    for rt_info in active_instances {
        active_log_paths.insert(rt_info.mount_config.common.log_file_path.as_path());
    }

    // Calculate retention count
    let active_count = active_log_paths.len();
    let retention_count = std::cmp::max(active_count, LOG_RETENTION_COUNT - 1);

    // Collect all log files matching the cleanup pattern
    let mut log_files = collect_files_with_mtime(log_dir, |fname| {
        (fname.starts_with("anylinuxfs-") || fname.starts_with("anylinuxfs."))
            && fname.ends_with(".log")
    });

    // Sort by modification time (newest first)
    log_files.sort_by(|a, b| b.1.cmp(&a.1));

    // Delete old files, keeping only the newest ones
    for (idx, (path, _)) in log_files.iter().enumerate() {
        if idx >= retention_count {
            // Only delete if it's not an active instance's log
            if !active_log_paths.contains(path.as_path()) {
                let _ = fs::remove_file(path);

                // Also delete corresponding kernel and nethelper logs
                if let Some(filename) = path.file_name() {
                    if let Some(fname_str) = filename.to_str() {
                        // Extract log_file_id from filename: "anylinuxfs-{ID}.log" -> "{ID}"
                        if let Some(id_part) = fname_str
                            .strip_prefix("anylinuxfs")
                            .and_then(|s| s.strip_suffix(".log"))
                        {
                            let kernel_log =
                                log_dir.join(format!("anylinuxfs_kernel{}.log", id_part));
                            let nethelper_log =
                                log_dir.join(format!("anylinuxfs_nethelper{}.log", id_part));
                            let _ = fs::remove_file(&kernel_log);
                            let _ = fs::remove_file(&nethelper_log);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Find the most recently modified log file matching a pattern in a directory
pub(crate) fn find_latest_log(
    log_dir: &Path,
    pattern_start: &str,
    pattern_end: &str,
) -> Option<PathBuf> {
    let mut log_files = collect_files_with_mtime(log_dir, |fname| {
        fname.starts_with(pattern_start) && fname.ends_with(pattern_end)
    });

    if log_files.is_empty() {
        return None;
    }

    // Sort by modification time (newest first) and get the latest
    log_files.sort_by(|a, b| b.1.cmp(&a.1));
    Some(log_files[0].0.clone())
}

/// Picks a unique hostname by appending a numeric suffix if needed.
pub(crate) fn pick_unique_hostname(base: &str, active_vm_hosts: &HashSet<String>) -> String {
    let base_val = base.to_owned();
    let mut counter = 0u32;
    loop {
        let candidate = if counter == 0 {
            base_val.clone()
        } else {
            let suffix = format!("-{}", counter);
            let truncated = &base_val[..base_val.len().min(63 - suffix.len())];
            format!("{}{}", truncated, suffix)
        };
        if !active_vm_hosts.contains(&format!("{}.local", candidate)) {
            return candidate;
        }
        counter += 1;
    }
}

/// Reads PTY output from the VM, parses `<anylinuxfs-*>` tags, and dispatches
/// NFS-ready / passphrase-prompt / report events to the parent process via channels.
struct PtyReader {
    pty_fd: libc::c_int,
    guest_prefix: log::Prefix,
    verbose: bool,
    config: MountConfig,
    vm_native_ip: Option<Ipv4Addr>,
    nfs_ready_tx: mpsc::Sender<NfsStatus>,
    vm_pwd_prompt_tx: mpsc::Sender<bool>,
    vm_report_tx: mpsc::Sender<vmctrl::Report>,
}

impl PtyReader {
    fn spawn(self) {
        _ = thread::spawn(move || {
            let mut nfs_ready = false;
            let mut fslabel: Option<String> = None;
            let mut fstype: Option<String> = None;
            let mut changed_to_ro = false;
            let mut exit_code = None;
            let mut buf_reader = PassthroughBufReader::new(
                unsafe { File::from_raw_fd(self.pty_fd) },
                self.guest_prefix,
            );
            let mut line = String::new();
            let mut exports = BTreeSet::new();

            loop {
                let bytes = match buf_reader.read_line(&mut line) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        host_eprintln!("Error reading from pty: {}", e);
                        break;
                    }
                };
                if bytes == 0 {
                    break; // EOF
                }
                if line.contains("READY AND WAITING FOR NFS CLIENT CONNECTIONS") {
                    self.nfs_ready_tx
                        .send(NfsStatus::Ready(NfsReadyState {
                            fslabel: fslabel.take(),
                            fstype: fstype.take(),
                            changed_to_ro,
                            exports: exports.iter().cloned().collect(),
                        }))
                        .unwrap();
                    nfs_ready = true;
                } else if line.starts_with("<anylinuxfs-vmproxy-ready>") {
                    subscribe_to_vm_events(
                        &self.config,
                        self.vm_native_ip,
                        self.vm_report_tx.clone(),
                    );
                } else if line.starts_with("<anylinuxfs-exit-code") {
                    exit_code = parse_vm_tag_value(&line).and_then(|v| v.parse::<i32>().ok());
                } else if line.starts_with("<anylinuxfs-label") {
                    fslabel = parse_vm_tag_value(&line).map(str::to_string);
                } else if line.starts_with("<anylinuxfs-type") {
                    fstype = parse_vm_tag_value(&line).map(str::to_string);
                } else if line.starts_with("<anylinuxfs-mount:changed-to-ro>") {
                    changed_to_ro = true;
                } else if line.starts_with("<anylinuxfs-nfs-export") {
                    if let Some(export_path) = parse_vm_tag_value(&line) {
                        exports.insert(export_path.to_string());
                    }
                } else if line.starts_with("<anylinuxfs-passphrase-prompt:start>") {
                    self.vm_pwd_prompt_tx.send(true).unwrap();
                } else if line.starts_with("<anylinuxfs-passphrase-prompt:end>") {
                    self.vm_pwd_prompt_tx.send(false).unwrap();
                } else if !self.verbose && line.starts_with("<anylinuxfs-force-output:off>") {
                    log::disable_console_log();
                } else if !self.verbose && line.starts_with("<anylinuxfs-force-output:on>") {
                    log::enable_console_log();
                }

                line.clear();
            }
            if !nfs_ready {
                self.nfs_ready_tx
                    .send(NfsStatus::Failed(exit_code))
                    .unwrap();
            }
        });
    }
}

/// Spawn a thread to connect to the VM control socket and subscribe to events.
fn subscribe_to_vm_events(
    config: &MountConfig,
    vm_native_ip: Option<Ipv4Addr>,
    vm_report_tx: mpsc::Sender<vmctrl::Report>,
) {
    let config = config.clone();
    _ = thread::spawn(move || {
        let Ok(mut stream) =
            vm_network::connect_to_vm_ctrl_socket(&config.common, vm_native_ip, None)
        else {
            return;
        };

        if let Err(e) = ipc::Client::write_request(&mut stream, &vmctrl::Request::SubscribeEvents) {
            host_eprintln!("Failed to send SubscribeEvents to vmctrl: {:#}", e);
            return;
        };

        match ipc::Client::read_response(&mut stream) {
            Ok(response) => {
                if let vmctrl::Response::ReportEvent(info) = response {
                    if let Err(e) = vm_report_tx.send(info) {
                        host_eprintln!("Failed to send VM report to channel: {:#}", e);
                    }
                }
            }
            Err(e) => {
                host_eprintln!("Failed to read VM report from vmctrl: {:#}", e);
            }
        }
    });
}

/// Set up rpcbind services for NFS when using GvProxy and host rpcbind is running.
/// `services_to_restore` must outlive `deferred` so cleanup can reference it.
fn setup_rpcbind_services<'a>(
    config: &MountConfig,
    network_env: &NetworkEnv,
    services_to_restore: &'a [rpcbind::Entry],
    deferred: &mut Deferred<'a>,
) -> anyhow::Result<()> {
    if !(config.common.net_helper == NetHelper::GvProxy && network_env.rpcbind_running) {
        return Ok(());
    }

    let is_host_rpcbind = !services_to_restore
        .iter()
        .any(|entry| entry.owner == "superuser");
    host_println!("is_host_rpcbind: {}", is_host_rpcbind);

    if !is_host_rpcbind {
        return Ok(());
    }

    _ = deferred.add(|| {
        rpcbind::services::unregister();
        _ = rpcbind::services::rpcb_set_entries(services_to_restore);
    });

    // if rpcbind is already running, we can use it to register our NFS server
    // but we have to unregister any conflicting system services first
    // (make sure to elevate if we need to unregister any services not owned by us)
    let unregister_fn = || -> anyhow::Result<()> {
        let uid = config.common.invoker_uid;
        if config.common.sudo_uid.is_none() && uid != 0 {
            let any_root_svcs = services_to_restore
                .iter()
                .any(|entry| Some(&entry.owner) != utils::user_name_from_uid(uid).as_ref());

            if any_root_svcs {
                safe_println!("rpcbind already running, need to use sudo for NFS setup")?;
                Command::new("sudo")
                    .arg("-S")
                    .arg(&config.common.exec_path)
                    .arg("rpcbind")
                    .arg("unregister")
                    .status()?;

                return Ok(());
            }
        }

        rpcbind::services::unregister();
        Ok(())
    };
    unregister_fn()?;

    // make sure to always run this as regular user
    // because cleanup code runs after we've dropped privileges
    // (regular user cannot unregister services registered by root)
    if let (Some(uid), Some(gid)) = (config.common.sudo_uid, config.common.sudo_gid) {
        let status = Command::new(&config.common.exec_path)
            .arg("rpcbind")
            .arg("register")
            .uid(uid)
            .gid(gid)
            .status()?;
        if !status.success() {
            return Err(anyhow!("Failed to register NFS server to rpcbind"));
        }
    } else {
        rpcbind::services::register().context("Failed to register NFS server to rpcbind")?;
    }

    Ok(())
}

/// Holds the NFS share path and mount options, and provides methods to mount
/// the primary share and any additional subdirectory exports.
struct NfsShareSetup<'a> {
    config: &'a MountConfig,
    vm_host_b: &'a [u8],
    share_path: BString,
    nfs_opts: fsutil::NfsOptions,
}

impl<'a> NfsShareSetup<'a> {
    /// Build the NFS share path and mount options from the config.
    fn new(
        config: &'a MountConfig,
        vm_host_b: &'a [u8],
        mnt_dev_info: &DevInfo,
        shared_volume: bool,
    ) -> Self {
        let share_name = match config.custom_mount_name() {
            Some(name) => name.as_bytes().into(),
            None => mnt_dev_info.auto_mount_name(),
        };

        let share_path = match config.get_action() {
            Some(action) if !action.override_nfs_export().is_empty() => {
                BString::from(action.override_nfs_export())
            }
            _ => [b"/mnt/", share_name.as_slice()].concat().into(),
        };

        let mut nfs_opts = fsutil::NfsOptions::default();
        if shared_volume {
            nfs_opts.remove("nolocks".as_bytes());
        }
        nfs_opts.extend(config.nfs_options.iter().map(|s| match s.split_once('=') {
            Some((key, value)) => (key.as_bytes().into(), value.as_bytes().into()),
            None => (s.as_bytes().into(), b"".into()),
        }));

        Self {
            config,
            vm_host_b,
            share_path,
            nfs_opts,
        }
    }

    fn mount(&self) -> anyhow::Result<()> {
        let mount_point: Cow<'_, _> = match self.config.custom_mount_point.as_deref() {
            // custom mount point must already exist
            Some(mount_point) => mount_point.into(),
            None => {
                // default mount point will be created
                let volume_base_dir = if self.config.common.sudo_uid.is_some() {
                    PathBuf::from("/")
                } else {
                    self.config.common.home_dir.clone()
                }
                .join("Volumes");

                let mut mount_name = self.share_path.split(|&b| b == b'/').last().unwrap();
                if mount_name.is_empty() {
                    mount_name = b"root";
                }
                let mut mount_path = volume_base_dir.join(Path::from_bytes(mount_name));
                let mut counter = 1;

                while mount_path.exists() {
                    mount_path = volume_base_dir.join(Path::from_bytes(
                        &[mount_name, b"-", counter.to_string().as_bytes()].concat(),
                    ));
                    counter += 1;
                }

                fs::create_dir_all(&mount_path).with_context(|| {
                    format!(
                        "Failed to create mount point directory {}",
                        mount_path.display()
                    )
                })?;
                chown(
                    &mount_path,
                    Some(self.config.common.invoker_uid),
                    Some(self.config.common.invoker_gid),
                )
                .with_context(|| format!("Failed to change owner of {}", mount_path.display()))?;

                mount_path.into()
            }
        };

        let shell_script = [
            b"mount -t nfs -o ",
            self.nfs_opts.to_list().as_slice(),
            b" \"",
            self.vm_host_b,
            b":",
            &self.share_path,
            b"\" \"",
            mount_point.as_bytes(),
            b"\"",
        ]
        .concat();

        let shell_script = OsStr::from_bytes(&shell_script);
        host_println!("NFS mount command: {}", shell_script.display());
        // try to run mount as regular user first
        // (if that succeeds, umount will work without sudo)
        let mut status = Command::new("sh")
            .arg("-c")
            .arg(shell_script)
            .uid(self.config.common.invoker_uid)
            .gid(self.config.common.invoker_gid)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;

        if !status.success() {
            // otherwise run as root (probably the mount point wasn't accessible)
            status = Command::new("sh").arg("-c").arg(shell_script).status()?;
        }

        if !status.success() {
            return Err(anyhow!(
                "failed with exit code {}",
                status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or("unknown".to_owned())
            ));
        }

        if self.config.open_finder {
            let mut shell_script =
            br#"afplay /System/Library/Components/CoreAudio.component/Contents/SharedSupport/SystemSounds/system/Volume\ Mount.aif \
                -v $(awk "BEGIN { print "$(osascript -e "get alert volume of (get volume settings)")"/100 };")"#.to_vec();
            shell_script.extend_from_slice(b" &! open \"");
            shell_script.extend_from_slice(mount_point.as_bytes());
            shell_script.extend_from_slice(b"\"");
            let shell_script = OsStr::from_bytes(&shell_script);
            let _ = Command::new("sh")
                .arg("-c")
                .arg(shell_script)
                .uid(self.config.common.invoker_uid)
                .gid(self.config.common.invoker_gid)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }

        Ok(())
    }

    /// Mount additional NFS subdirectory exports under the main mount point.
    fn mount_subdirectories(
        &self,
        exports: &[String],
        mount_point: &diskutil::MountPoint,
        verbose: bool,
    ) {
        let mut additional_exports = exports
            .iter()
            .map(|item| item.as_str())
            .filter(|&export_path| export_path != self.share_path)
            .peekable();

        let _log_guard = ConsoleLogGuard::enable_temporarily(verbose);
        let elevate = self.config.common.sudo_uid.is_none() && self.config.common.invoker_uid != 0;

        if elevate && additional_exports.peek().is_some() {
            host_println!("need to use sudo to mount additional NFS exports");
        }
        match fsutil::mount_nfs_subdirs(
            self.vm_host_b,
            &self.share_path,
            additional_exports.into_iter(),
            mount_point.display(),
            &self.nfs_opts,
            elevate,
        ) {
            Ok(_) => {}
            Err(e) => host_eprintln!("Failed to mount additional NFS exports: {:#}", e),
        }
    }
}

/// Build the list of passphrase prompt callbacks for encrypted devices.
///
/// Depending on the passphrase prompt config, one callback per encrypted device
/// or a single shared callback is created. Also bumps RAM allocation for LUKS.
fn prepare_passphrase_callbacks(
    dev_info: &[DevInfo],
    config: &mut MountConfig,
    env_has_passphrase: bool,
) -> Vec<Box<dyn FnOnce()>> {
    let mut callbacks: Vec<Box<dyn FnOnce()>> = Vec::new();
    let mut passphrase_needed = false;

    if !env_has_passphrase && config.key_file.is_none() {
        for di in dev_info {
            let is_luks = di.fs_type() == Some("crypto_LUKS");
            if di.fs_type().is_some_and(common_utils::is_encrypted_fs) {
                if is_luks {
                    ensure_enough_ram_for_luks(&mut config.common);
                }
                if config.common.passphrase_config == PassphrasePromptConfig::AskForEach {
                    let disk = di.disk().to_owned();
                    let prompt_fn = diskutil::passphrase_prompt(Some(disk));
                    callbacks.push(Box::new(prompt_fn));
                }
                passphrase_needed = true;
            }
        }
        if passphrase_needed && config.common.passphrase_config == PassphrasePromptConfig::OneForAll
        {
            let prompt_fn = diskutil::passphrase_prompt(None);
            callbacks.push(Box::new(prompt_fn));
        }
    }

    callbacks
}

impl super::AppRunner {
    pub(crate) fn run_mount(&mut self, cmd: MountCmd) -> anyhow::Result<()> {
        let _lock_file = LockFile::new(LOCK_FILE)?.acquire_lock(FlockKind::Shared)?;
        let mut network_env = NetworkEnv::default();

        if let Err(e) = netutil::try_port((Ipv4Addr::from([0, 0, 0, 0]), 111))
            && e.kind() == io::ErrorKind::AddrInUse
        {
            network_env.rpcbind_running = true;
        }

        let config = load_mount_config(cmd)?;
        // verify if mount can be executed without a disk
        if config.disk_path.is_empty()
            && match config.get_action() {
                Some(action) => action.override_nfs_export().is_empty(),
                None => true,
            }
        {
            return Err(anyhow!(
                "mount with no disk isn't valid unless a custom action with NFS export override is specified"
            ));
        }

        let log_file_path = &config.common.log_file_path;

        // Discover active instances once, to avoid redundant socket polling
        let (active_instances, stale_sockets) = collect_active_instances();
        for socket in stale_sockets {
            let _ = fs::remove_file(socket);
        }

        // Clean up old log files before initializing new log
        if let Some(log_dir) = log_file_path.parent() {
            _ = cleanup_old_logs(log_dir, active_instances.iter());
        }

        network_env.active_vm_hosts = active_instances
            .iter()
            .filter_map(|rt| String::from_utf8(rt.vm_host.clone()).ok())
            .collect();

        log::init_log_file(log_file_path).context("Failed to create log file")?;
        // Change owner to invoker_uid and invoker_gid

        chown(
            log_file_path,
            Some(config.common.invoker_uid),
            Some(config.common.invoker_gid),
        )
        .context(format!(
            "Failed to change owner of {}",
            log_file_path.display(),
        ))?;

        // remove kernel log from the last run
        _ = fs::remove_file(&config.common.kernel_log_file_path);

        let forked = utils::fork_with_comm_pipe()?;
        if forked.pid == 0 {
            self.is_child = true;
            let verbose = config.verbose;
            let res = self.run_mount_child(config, network_env, forked.comm_fd());
            if res.is_err() {
                if !verbose {
                    self.print_log = true;
                }
                unsafe { write_to_pipe(forked.comm_fd(), b"join\n") }
                    .context("Failed to write to pipe")?;
            }
            res
        } else {
            self.run_mount_parent(forked)
        }
    }

    pub(crate) fn run_mount_child(
        &mut self,
        mut config: MountConfig,
        mut network_env: NetworkEnv,
        comm_write_fd: libc::c_int,
    ) -> anyhow::Result<()> {
        // pre-declare so it can be referenced in a deferred action
        let stdin_forwarder;
        let services_to_restore: Vec<_>;
        let vm_native_ip;
        let api_socket_path: String;
        let mut deferred = Deferred::new();

        let verbose = config.verbose;
        if !verbose {
            log::disable_console_log();
        }

        #[allow(unused_mut)]
        let (mut dev_info, mut mnt_dev_info, _disks) = claim_devices(&mut config)?;

        #[allow(unused_mut)]
        let mut opts = VMOpts::new()
            .read_only_disks(config.read_only)
            .read_only_root(!config.common.rw_rootfs);

        #[allow(unused_mut)]
        let mut img_src = ImageSource::default();

        // Use FreeBSD when the filesystem requires or prefers it, and no
        // incompatible custom action has been specified.
        #[cfg(feature = "freebsd")]
        if mnt_dev_info
            .fs_type()
            .map(|fs| config.common.fs_preferred_os(fs) == OSType::FreeBSD)
            .unwrap_or(false)
            && config
                .get_action()
                .map(|a| a.required_os())
                .flatten()
                .unwrap_or(OSType::FreeBSD)
                == OSType::FreeBSD
        {
            let bsd_image = config
                .common
                .preferences
                .default_image(OSType::FreeBSD)
                .unwrap_or("freebsd-15.0");

            let src = config
                .common
                .preferences
                .images()
                .get(bsd_image)
                .map(|&s| s.to_owned())
                .with_context(|| format!("FreeBSD image {} not found", bsd_image))?;
            mnt_dev_info.set_vm_disk("/dev/vtbd1".to_string());

            config = config.with_image_source(&src);
            let freebsd_base_path = config.common.profile_path.join(&src.base_dir);
            let vm_disk_image = "freebsd-microvm-disk.img";
            let root_disk_path = freebsd_base_path.join(vm_disk_image);
            host_println!("root_disk: {}", root_disk_path.display());

            opts = opts.root_device("ufs:/dev/gpt/rootfs").legacy_console(true);
            dev_info = [DevInfo::pv(root_disk_path.as_bytes(), true)?]
                .iter()
                .chain(dev_info.iter())
                .cloned()
                .collect();

            img_src = src;
        } else {
            host_println!("root_path: {}", config.common.root_path.display());
        }

        {
            let _log_guard = ConsoleLogGuard::enable_temporarily(verbose);
            vm_image::init(&config.common, false, &img_src)?;
        }

        let (vm_env, env_has_passphrase) = prepare_vm_environment(&config)?;

        host_println!("num_vcpus: {}", config.common.preferences.krun_num_vcpus());
        host_println!(
            "ram_size_mib: {}",
            config.common.preferences.krun_ram_size_mib()
        );

        // if this is NTFS or exFAT, we add uid/gid mount options
        if let Some(fs_type) = mnt_dev_info.fs_type()
            && diskutil::WINDOWS_LABELS
                .fs_types
                .iter()
                .cloned()
                .any(|t| t == fs_type)
        {
            let mut opts = config.mount_options.unwrap_or_default();
            if !opts.is_empty() {
                opts.push_str(",");
            }
            opts.push_str(&format!(
                "uid={},gid={}",
                config.common.invoker_uid, config.common.invoker_gid
            ));
            config.mount_options = Some(opts);
        }

        let passphrase_callbacks =
            prepare_passphrase_callbacks(&dev_info, &mut config, env_has_passphrase);

        let mut can_detach = true;
        let session_pgid = unsafe { libc::setsid() };
        if session_pgid < 0 {
            host_eprintln!("Failed to setsid, cannot run in the background");
            can_detach = false;
        }

        let os = config.common.kernel.os;
        let shared_volume = config.bind_addr.is_some_and(|addr| !addr.is_loopback());

        config.common.net_helper = config
            .common
            .net_helper
            .bind_addr_override(shared_volume)
            .os_override(os);

        let (mut net_helper_proc, net_helper_name, vmnet_config, vm_ip) =
            match config.common.net_helper {
                NetHelper::GvProxy => {
                    let loopback_ip = netutil::pick_usable_loopback_ip(&[2049, 32765, 32767])?;
                    network_env.usable_loopback_ip = Some(loopback_ip.clone());
                    (
                        vm_network::start_gvproxy(&config.common)?,
                        "gvproxy",
                        None,
                        loopback_ip,
                    )
                }
                NetHelper::VmNet => {
                    let (child, vmnet_cfg) = vm_network::start_vmnet_helper(&config.common)?;
                    let vm_ip = vmnet_cfg.vm_ip();
                    (
                        child,
                        "vmnet-helper",
                        Some(vmnet_cfg),
                        Host::from_ip(IpAddr::V4(vm_ip), None),
                    )
                }
            };

        vm_native_ip = vmnet_config.as_ref().map(|cfg| cfg.vm_ip());

        let net_helper_pid = net_helper_proc.id() as libc::pid_t;
        fsutil::wait_for_file(&config.common.unixgram_sock_path)?;

        _ = deferred.add({
            let vfkit_sock_path = config.common.unixgram_sock_path.clone();
            move || {
                if let Err(e) = vm_network::vfkit_sock_cleanup(&vfkit_sock_path) {
                    host_eprintln!("{:#}", e);
                }
            }
        });

        if let Some(status) = net_helper_proc.try_wait().ok().flatten() {
            return Err(anyhow!(
                "{} failed with exit code: {}",
                net_helper_name,
                status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or("unknown".to_owned())
            ));
        }

        _ = deferred.add(move || {
            if let Err(e) = terminate_child(&mut net_helper_proc, net_helper_name) {
                host_eprintln!("{:#}", e);
            }
        });

        _ = deferred.add({
            let vsock_path = config.common.vsock_path.clone();
            move || {
                if let Err(e) = vm_network::vsock_cleanup(&vsock_path) {
                    host_eprintln!("{:#}", e);
                }
            }
        });

        // Prepare the key file for transfer into the VM (runs in the parent before forking).
        // Cleanup is registered in `deferred` and fires after the child exits.
        let prepared_key_file = prepare_key_file_for_vm(
            config.key_file.as_deref(),
            os,
            &config.common,
            &mut deferred,
        )
        .context("Failed to prepare key file for VM")?;

        let mut forked = utils::fork_with_pty_output(OutputAction::RedirectLater)?;
        if forked.pid == 0 {
            // Child process
            deferred.remove_all(); // deferred actions must be only called in the parent process

            let ctx = setup_vm(
                &config.common,
                &dev_info,
                NetworkMode::default_virtio_net(os, config.common.net_helper, vmnet_config),
                true,
                opts,
            )
            .context("Failed to setup microVM")?;

            let to_decrypt: Vec<_> = iter::zip(dev_info.iter(), 'a'..='z')
                .filter_map(|(di, letter)| {
                    if di.fs_type().is_some_and(common_utils::is_encrypted_fs) {
                        Some(format!("/dev/vd{}", letter))
                    } else {
                        None
                    }
                })
                .collect();

            start_vmproxy(
                &ctx,
                &config,
                &network_env,
                &vm_env,
                &mnt_dev_info,
                dev_info.len() > 1,
                to_decrypt,
                &prepared_key_file,
                || forked.redirect(),
            )
            .context("Failed to start microVM")?;
        } else {
            // Parent process
            let child_pid = forked.pid;
            let vm_wait_action = deferred.add(move || {
                _ = wait_for_vm_status(child_pid);
            });

            let signal_hub = utils::start_signal_publisher()?;

            // DNS record must be created by regular user, otherwise
            // dropping permissions after mount won't have any effect
            drop_effective_privileges(config.common.sudo_uid, config.common.sudo_gid)?;

            // Pick a hostname not already in use by another anylinuxfs instance
            config.vm_hostname =
                pick_unique_hostname(&config.vm_hostname, &network_env.active_vm_hosts);

            let vm_fqdn = format!("{}.local", &config.vm_hostname);
            let conn = DNSService::create_connection().unwrap();
            // vm_dns_rec must remain in scope for the duration of the mount, otherwise the DNS record will be removed
            let mut vm_dns_rec: Option<DNSRecord> = conn
                .register_record(&vm_fqdn, vm_ip.with_port(0)?, Some("lo0"))
                .inspect_err(|e| eprintln!("DNS registration error: {e}"))
                .ok();

            let vm_host = if vm_dns_rec.is_some() {
                Host::new(&vm_fqdn)
            } else {
                vm_ip
            };

            let vm_host_b = vm_host.to_string().into_bytes();

            // Generate unique API socket path for this instance
            api_socket_path = format!("/tmp/anylinuxfs-{}.sock", rand_string(8));

            let rt_info = Arc::new(Mutex::new(api::RuntimeInfo {
                mount_config: config.clone(),
                dev_info: mnt_dev_info.clone(),
                session_pgid,
                vmm_pid: child_pid,
                net_helper_pid,
                vm_host: vm_host_b.to_vec(),
                vm_native_ip,
                mount_point: None,
            }));

            api::serve_info(rt_info.clone(), api_socket_path.clone());

            _ = deferred.add(move || {
                if let Err(e) = fs::remove_file(&api_socket_path) {
                    if e.kind() != io::ErrorKind::NotFound {
                        host_eprintln!(
                            "Error removing API socket file {}: {}",
                            &api_socket_path,
                            e
                        );
                    }
                }
            });

            services_to_restore =
                if config.common.net_helper == NetHelper::GvProxy && network_env.rpcbind_running {
                    rpcbind::services::list()?
                        .into_iter()
                        .filter(|entry| {
                            entry.prog == rpcbind::RPCPROG_MNT
                                || entry.prog == rpcbind::RPCPROG_NFS
                                || entry.prog == rpcbind::RPCPROG_STAT
                        })
                        .collect()
                } else {
                    Vec::new()
                };
            setup_rpcbind_services(&config, &network_env, &services_to_restore, &mut deferred)?;

            let (nfs_ready_tx, nfs_ready_rx) = mpsc::channel();
            let (vm_pwd_prompt_tx, vm_pwd_prompt_rx) = mpsc::channel();
            let (vm_report_tx, vm_report_rx) = mpsc::channel::<vmctrl::Report>();

            let kernel_log_file_path = config.common.kernel_log_file_path.as_path();
            deferred.add(move || {
                match vm_report_rx.recv_timeout(Duration::from_secs(3)) {
                    Ok(report) => {
                        // save report to a file
                        host_println!("VM report received");
                        match fs::write(kernel_log_file_path, report.kernel_log) {
                            Ok(_) => {
                                host_println!(
                                    "Kernel log saved to {}",
                                    kernel_log_file_path.display()
                                );
                            }
                            Err(e) => {
                                host_eprintln!(
                                    "Failed to save kernel log to {}: {}",
                                    kernel_log_file_path.display(),
                                    e
                                );
                            }
                        }
                    }
                    Err(e) => {
                        host_eprintln!("Failed to receive VM report: {}", e);
                    }
                }
            });

            let guest_prefix = match config.common.kernel.os {
                OSType::Linux => log::Prefix::GuestLinux,
                OSType::FreeBSD => log::Prefix::GuestBSD,
            };

            PtyReader {
                pty_fd: forked.master_fd(),
                guest_prefix,
                verbose,
                config: config.clone(),
                vm_native_ip,
                nfs_ready_tx,
                vm_pwd_prompt_tx,
                vm_report_tx,
            }
            .spawn();

            let signals = signal_hub.subscribe();
            let signal_subscr_id = signals.id().expect("just subscribed, ID should be set");
            stdin_forwarder = utils::StdinForwarder::new(forked.master_fd(), signals)?;
            let disable_stdin_fwd_action = deferred.add(|| {
                if let Err(e) = stdin_forwarder.stop() {
                    host_eprintln!("{:#}", e);
                }
            });

            stdin_forwarder.echo_newline(true);
            for passphrase_fn in passphrase_callbacks {
                // wait for the VM to prompt for passphrase
                vm_pwd_prompt_rx.recv().unwrap_or(false);
                passphrase_fn();
                // wait for the passphrase to be entered
                vm_pwd_prompt_rx.recv().unwrap_or(false);
            }
            stdin_forwarder.echo_newline(false);

            let nfs_status =
                wait_for_nfs_server(vm_host.raw_str(), 2049, &mut vm_dns_rec, nfs_ready_rx)
                    .inspect_err(|e| {
                        host_eprintln!("Error waiting for NFS server: {:#}", e);
                    })
                    .unwrap_or(NfsStatus::Failed(None));

            // we need original permissions (possibly root) to execute mount_nfs
            elevate_effective_privileges()?;

            if let NfsStatus::Ready(NfsReadyState {
                fslabel,
                fstype,
                changed_to_ro,
                exports,
            }) = &nfs_status
            {
                host_println!("Port 2049 open, NFS server ready");

                // from now on, if anything fails, we need to send quit command to the VM
                let quit_action = deferred.add(|| {
                    _ = send_quit_cmd(&config.common, vm_native_ip);
                });

                // once the NFS server is ready, we need to change how termination signals are handled
                // EventSession is going to subscribe to signals, so we unsubscribe the previous handler first
                signal_hub.unsubscribe(signal_subscr_id);
                let signals = signal_hub.subscribe();
                let event_session = diskutil::EventSession::new(signals)?;

                if let Some(label) = fslabel {
                    mnt_dev_info.set_label(label);
                    rt_info.lock().unwrap().dev_info.set_label(label);
                }

                if let Some(fstype) = fstype {
                    mnt_dev_info.set_fs_type(fstype);
                    rt_info.lock().unwrap().dev_info.set_fs_type(fstype);
                }

                if *changed_to_ro {
                    rt_info.lock().unwrap().mount_config.read_only = true;
                    let mount_opts = rt_info.lock().unwrap().mount_config.mount_options.clone();
                    let new_mount_opts = mount_opts
                        .map(|opts| format!("ro,{}", opts))
                        .unwrap_or("ro".into());
                    rt_info.lock().unwrap().mount_config.mount_options = Some(new_mount_opts);
                }

                let nfs_share =
                    NfsShareSetup::new(&config, &vm_host_b, &mnt_dev_info, shared_volume);

                let mount_result = nfs_share.mount();
                match &mount_result {
                    Ok(_) => host_println!("Requested NFS share mount"),
                    Err(e) => {
                        let _log_guard = ConsoleLogGuard::enable_temporarily(verbose);
                        host_eprintln!("Failed to request NFS mount: {:#}", e);
                    }
                };

                let mount_point_opt = if mount_result.is_ok() {
                    let nfs_path =
                        PathBuf::from(format!("{}:{}", vm_host_b.as_bstr(), nfs_share.share_path));
                    event_session.wait_for_mount(&nfs_path)
                } else {
                    None
                };

                deferred.call_now(disable_stdin_fwd_action);

                if let Some(mount_point) = &mount_point_opt {
                    let mut disk: String = mnt_dev_info.disk().display().to_string();
                    if disk.is_empty() {
                        disk = "<unknown>".into();
                    }
                    host_println!("{} was mounted as {}", disk, mount_point.display());

                    if config.custom_mount_point.is_none() {
                        // mount point will be removed only if it was auto-created
                        let mnt_point_path = PathBuf::from(mount_point.display());
                        deferred.add(move || {
                            if mnt_point_path.exists() {
                                host_println!("Removing mount point {}", mnt_point_path.display());
                                _ = fs::remove_dir(&mnt_point_path);
                            }
                        });
                    }

                    rt_info.lock().unwrap().mount_point = Some(mount_point.display().into());
                    nfs_share.mount_subdirectories(exports, mount_point, verbose);
                }

                // drop privileges back to the original user if he used sudo
                drop_privileges(config.common.sudo_uid, config.common.sudo_gid)?;

                if can_detach {
                    // tell the parent to detach from console (i.e. exit)
                    unsafe { write_to_pipe(comm_write_fd, b"detach\n") }
                        .context("Failed to write to pipe")?;

                    // stop printing to the console
                    log::disable_console_log();
                } else {
                    // tell the parent to wait for the child to exit
                    unsafe { write_to_pipe(comm_write_fd, b"join\n") }
                        .context("Failed to write to pipe")?;
                }

                if let Some(mount_point) = &mount_point_opt {
                    event_session.wait_for_unmount(mount_point.real());
                    host_println!("Share {} was unmounted", mount_point.display());
                }
                deferred.remove(quit_action);
                send_quit_cmd(&config.common, vm_native_ip)?;
            } else {
                host_println!("NFS server not ready");

                // drop privileges back to the original user if he used sudo
                drop_privileges(config.common.sudo_uid, config.common.sudo_gid)?;

                // tell the parent to wait for the child to exit
                unsafe { write_to_pipe(comm_write_fd, b"join\n") }
                    .context("Failed to write to pipe")?;
            }

            deferred.remove(vm_wait_action);
            if let Some(mut status) = wait_for_vm_status(child_pid)? {
                if status == 0 {
                    if let NfsStatus::Failed(Some(exit_code)) = nfs_status {
                        status = exit_code;
                    }
                }

                if status != 0 {
                    return Err(StatusError::new("VM exited with status", status).into());
                }
            }
        }

        Ok(())
    }

    pub(crate) fn run_mount_parent(
        &mut self,
        forked: utils::ForkOutput<(), (), CommFd>,
    ) -> anyhow::Result<()> {
        let comm_read_fd = forked.comm_fd();
        let mut buf_reader = BufReader::new(unsafe { File::from_raw_fd(comm_read_fd) });
        let mut line = String::new();
        while let Ok(bytes) = buf_reader.read_line(&mut line) {
            let cmd = line.trim();
            // host_println!("DEBUG pipe cmd: '{}'", cmd);

            if bytes == 0 || cmd == "join" {
                let mut status = 0;
                if unsafe { libc::waitpid(forked.pid, &mut status, 0) } < 0 {
                    return Err(io::Error::last_os_error())
                        .context("Failed to wait for child process");
                }
                match status {
                    0 => return Ok(()),
                    _ => return Err(StatusError::new("exited with status", status).into()),
                }
            }

            if cmd == "detach" {
                // child is signalling it will continue to run
                // in the background; we can exit without waiting
                break;
            }

            line.clear();
        }
        Ok(())
    }

    pub(crate) fn run_unmount(&mut self, cmd: UnmountCmd) -> anyhow::Result<()> {
        let (active_instances, _) = collect_active_instances();

        for rt_info in active_instances {
            // If a path was specified, check if this instance matches
            if let Some(ref target_path) = cmd.path {
                let target_path =
                    fs::canonicalize(target_path).unwrap_or_else(|_| PathBuf::from(target_path));
                let matches_disk = target_path == Path::new(&rt_info.mount_config.disk_path);
                let matches_mount_point = rt_info
                    .mount_point
                    .as_ref()
                    .map(|mp| target_path == Path::new(mp))
                    .unwrap_or(false);
                let matches_disk_part = rt_info
                    .mount_config
                    .disk_path
                    .split(':')
                    .any(|p| OsStr::new(p) == target_path.as_os_str());

                if !matches_disk && !matches_mount_point && !matches_disk_part {
                    continue;
                }
            }

            let mount_point = match validated_mount_point(&rt_info) {
                MountStatus::Mounted(mount_point) => mount_point,
                MountStatus::NoLonger => {
                    eprintln!(
                        "Drive {} no longer mounted but anylinuxfs is still running; try `anylinuxfs stop`.",
                        &rt_info.mount_config.disk_path
                    );
                    continue;
                }
                MountStatus::NotYet => {
                    eprintln!(
                        "Drive {} not mounted yet, please wait",
                        &rt_info.mount_config.disk_path
                    );
                    continue;
                }
            };

            let mount_table = fsutil::MountTable::new()?;
            let our_mount_points = mount_table
                .mount_points()
                .map(|item| item.as_os_str())
                .filter(|&mpt| mpt.as_bytes().starts_with(mount_point.as_bytes()));

            fsutil::unmount_nfs_subdirs(our_mount_points, &mount_point)?;

            if cmd.wait_for_vm {
                wait_for_proc_exit_with_timeout(rt_info.session_pgid, Duration::from_secs(20))?;
            }

            // If a specific path was requested, we're done
            if cmd.path.is_some() {
                break;
            }
        }

        Ok(())
    }
}
