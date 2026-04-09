use anyhow::{Context, anyhow};
use bstr::{BString, ByteSlice};
use clap::Parser;
#[cfg(target_os = "linux")]
use common_utils::FromPath;
#[cfg(any(target_os = "freebsd", target_os = "macos"))]
use common_utils::VM_CTRL_PORT;
use common_utils::{
    CustomActionConfig, Deferred, VM_GATEWAY_IP, VM_IP, ipc, path_safe_label_name, vmctrl,
};
use ipnet::Ipv4Net;
#[cfg(target_os = "linux")]
use libc::VMADDR_CID_ANY;
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};
#[cfg(target_os = "linux")]
use std::ffi::CString;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read, Write};
#[cfg(any(target_os = "freebsd", target_os = "macos"))]
use std::net::TcpListener;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use std::{env, thread};
#[cfg(target_os = "linux")]
use sys_mount::{UnmountFlags, unmount};
#[cfg(target_os = "linux")]
use vsock::{VsockAddr, VsockListener};

use crate::utils::{script, script_output};

mod kernel_cfg;
mod utils;
mod zfs;

#[derive(Parser)]
#[command(version, about, long_about = None)]
#[clap(disable_help_flag = true)]
struct Cli {
    disk_path: String,
    mount_name: String,
    #[arg(short, long)]
    custom_mount_point: bool, // if true, mount_name is not a hint but override
    #[arg(short = 't', long = "types")]
    fs_type: Option<String>,
    #[arg(long = "fs-driver")]
    fs_driver: Option<String>,
    #[arg(short = 'o', long = "options")]
    mount_options: Option<String>,
    #[arg(short, long)]
    decrypt: Option<String>,
    #[arg(long)]
    assemble_raid: bool,
    #[arg(short, long)]
    action: Option<String>,
    /// Path to the key file inside the VM
    #[arg(long = "key-file")]
    key_file: Option<String>,
    #[arg(long = "nfs-export-opts")]
    nfs_export_opts: Option<String>,
    #[arg(long = "ignore-permissions")]
    ignore_permissions: bool,
    #[arg(short, long, value_delimiter = ',', num_args = 0..)]
    bind_addrs: Vec<String>,
    #[arg(short, long)]
    multi_device: bool,
    #[arg(short, long)]
    reuse_passphrase: bool,
    #[arg(short, long)]
    host_rpcbind: bool,
    #[arg(short, long)]
    native_network: Option<Ipv4Net>,
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Serialize, Debug)]
struct PortDef<'a> {
    local: &'a str,
    remote: &'a str,
}

fn expose_port(client: &reqwest::blocking::Client, port_def: &PortDef) -> anyhow::Result<()> {
    client
        .post(&format!("http://{VM_GATEWAY_IP}/services/forwarder/expose"))
        .json(port_def)
        .send()
        .and_then(|res| res.error_for_status())
        .context(format!("Failed to expose port: {:?}", port_def))?;

    Ok(())
}

fn init_network(
    bind_addrs: &[String],
    host_rpcbind: bool,
    native_network: Option<Ipv4Net>,
) -> anyhow::Result<()> {
    // resolv.conf is already initialized and always the same on FreeBSD
    #[cfg(target_os = "linux")]
    fs::write("/tmp/resolv.conf", format!("nameserver {VM_GATEWAY_IP}\n"))
        .context("Failed to write /tmp/resolv.conf")?;

    let vm_gateway_ip = native_network
        .map(|net| net.hosts().next())
        .flatten()
        .unwrap_or(
            VM_GATEWAY_IP
                .parse()
                .context("Failed to parse VM_GATEWAY_IP")?,
        );

    let vm_ip = native_network
        .map(|net| net.hosts().nth(1))
        .flatten()
        .unwrap_or(VM_IP.parse().context("Failed to parse VM_IP")?);

    let net_prefix_len = native_network.map(|net| net.prefix_len()).unwrap_or(24);

    #[cfg(target_os = "linux")]
    let script = format!(
        "ip addr add {vm_ip}/{net_prefix_len} dev eth0 \
            && ip link set eth0 up \
            && ip route add default via {vm_gateway_ip} dev eth0",
    );
    #[cfg(any(target_os = "freebsd", target_os = "macos"))]
    let script = format!(
        "ifconfig vtnet0 inet {vm_ip}/{net_prefix_len} \
            && route add default {vm_gateway_ip} \
            && ifconfig lo0 up",
    );

    Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .status()
        .context("Failed to configure network interface")?;

    if native_network.is_none() {
        let bind_addr_set: HashSet<_> = bind_addrs.iter().collect();
        let client = reqwest::blocking::Client::new();

        if !host_rpcbind {
            expose_port(
                &client,
                &PortDef {
                    local: ":111",
                    remote: &format!("{VM_IP}:111"),
                },
            )?;
        }

        for addr in bind_addr_set {
            expose_port(
                &client,
                &PortDef {
                    local: &format!("{addr}:2049"),
                    remote: &format!("{VM_IP}:2049"),
                },
            )?;
            expose_port(
                &client,
                &PortDef {
                    local: &format!("{addr}:32765"),
                    remote: &format!("{VM_IP}:32765"),
                },
            )?;
            expose_port(
                &client,
                &PortDef {
                    local: &format!("{addr}:32767"),
                    remote: &format!("{VM_IP}:32767"),
                },
            )?;
        }
    }

    Ok(())
}

#[cfg(target_os = "freebsd")]
fn setup_fs_overlay(dir: &str) -> anyhow::Result<()> {
    let status = script(&format!(
        "mount -t tmpfs tmpfs /overlay/{} && mount -t unionfs /overlay/{} /{}",
        dir, dir, dir
    ))
    .status()
    .context(format!("Failed to setup overlay for {}", dir))?;

    if !status.success() {
        return Err(anyhow!("Failed to setup overlay for {}", dir));
    }

    Ok(())
}

#[cfg(target_os = "freebsd")]
fn setup_writable_dirs_for_nfsd() -> anyhow::Result<()> {
    for dir in &["etc", "var"] {
        setup_fs_overlay(dir)?;
    }
    Ok(())
}

trait StreamListener: Send + Sync + 'static {
    fn incoming(&self) -> impl Iterator<Item = io::Result<impl Read + Write + Send>>;
}

#[cfg(target_os = "linux")]
impl StreamListener for VsockListener {
    fn incoming(&self) -> impl Iterator<Item = io::Result<impl Read + Write + Send>> {
        self.incoming()
    }
}

#[cfg(any(target_os = "freebsd", target_os = "macos"))]
impl StreamListener for TcpListener {
    fn incoming(&self) -> impl Iterator<Item = io::Result<impl Read + Write + Send>> {
        self.incoming()
    }
}

struct CtrlSocketServer {
    done_rx: mpsc::Receiver<()>,
    quit_rx: mpsc::Receiver<()>,
    report_tx: mpsc::Sender<vmctrl::Report>,
}

impl CtrlSocketServer {
    fn new(listener: impl StreamListener) -> Self {
        let (done_tx, done_rx) = mpsc::channel();
        let (quit_tx, quit_rx) = mpsc::channel();
        let (report_tx, report_rx) = mpsc::channel();

        _ = thread::spawn(move || {
            let done_tx = Arc::new(Mutex::new(Some(done_tx)));
            let report_rx = Arc::new(Mutex::new(Some(report_rx)));

            thread::scope(|s| {
                let events_subscribed = Arc::new(AtomicBool::new(false));

                for stream in listener.incoming() {
                    let mut stream = match stream {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("Failed to accept vmctrl connection: {}", e);
                            break;
                        }
                    };

                    let done_tx = Arc::clone(&done_tx);
                    let report_rx = Arc::clone(&report_rx);
                    let events_subscribed = Arc::clone(&events_subscribed);

                    if let Ok(cmd) = ipc::Handler::read_request(&mut stream) {
                        println!("Received command: '{:?}'", &cmd);
                        match cmd {
                            vmctrl::Request::Quit => {
                                _ = quit_tx.send(());
                                _ = ipc::Handler::write_response(
                                    &mut stream,
                                    &vmctrl::Response::Ack,
                                );
                                _ = stream.flush();

                                if !events_subscribed.load(Ordering::Relaxed) {
                                    if let Some(done_tx) = done_tx.lock().unwrap().take() {
                                        _ = done_tx.send(());
                                    }
                                }
                                break;
                            }
                            vmctrl::Request::SubscribeEvents => {
                                s.spawn(move || {
                                    events_subscribed.store(true, Ordering::Relaxed);

                                    if let Some(report_rx) = report_rx.lock().unwrap().take() {
                                        let report = report_rx.recv().map_or_else(
                                            |e| vmctrl::Report {
                                                kernel_log: e.to_string().into(),
                                            },
                                            |v| v,
                                        );
                                        if let Err(e) = ipc::Handler::write_response(
                                            &mut stream,
                                            &vmctrl::Response::ReportEvent(report),
                                        ) {
                                            eprintln!(
                                                "Failed to write VM report response: {:#}",
                                                e
                                            );
                                        } else {
                                            _ = stream.flush();
                                            println!("Sent report to vmctrl client");
                                        }
                                        if let Some(done_tx) = done_tx.lock().unwrap().take() {
                                            _ = done_tx.send(());
                                        }
                                    } else {
                                        eprintln!("Report channel already taken");
                                    }
                                });
                            }
                        }
                    }
                }
            });
        });

        Self {
            done_rx,
            quit_rx,
            report_tx,
        }
    }

    fn wait_for_quit_cmd(&self) {
        _ = self.quit_rx.recv();
    }

    fn send_report(&self, report: vmctrl::Report) -> anyhow::Result<()> {
        self.report_tx
            .send(report)
            .context("Failed to send report to ctrl socket")?;

        // wait for the thread that actually sends the data
        _ = self.done_rx.recv();
        Ok(())
    }
}

fn is_read_only_set<'a>(mut mount_options: impl Iterator<Item = &'a str>) -> bool {
    mount_options.any(|opt| opt == "ro")
}

fn terminate_child(child: &mut Child, child_name: &str) -> anyhow::Result<()> {
    common_utils::terminate_child(child, child_name, None)
}

struct CustomActionRunner {
    config: Option<CustomActionConfig>,
    env: HashMap<String, String>,
}

impl CustomActionRunner {
    pub fn new(config: Option<CustomActionConfig>) -> Self {
        Self {
            config,
            env: HashMap::new(),
        }
    }

    pub fn set_env(&mut self, key: impl Into<String>, value: String) {
        self.env.insert(key.into(), value);
    }

    fn execute_action(&self, command: impl AsRef<OsStr>) -> anyhow::Result<()> {
        let status = Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .envs(self.env.iter())
            .status()?;

        if !status.success() {
            return Err(anyhow!(
                "command failed with status: {}",
                status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or("unknown".to_owned())
            ));
        }
        Ok(())
    }

    pub fn before_mount(&self) -> anyhow::Result<()> {
        if let Some(action) = &self.config {
            if !action.before_mount().is_empty() {
                println!("<anylinuxfs-force-output:on>");
                println!("Running before_mount action: `{}`", action.before_mount());
                let result = self.execute_action(action.before_mount());
                println!("<anylinuxfs-force-output:off>");
                result?;
            }
        }
        Ok(())
    }

    pub fn after_mount(&self) -> anyhow::Result<()> {
        if let Some(action) = &self.config {
            if !action.after_mount().is_empty() {
                println!("<anylinuxfs-force-output:on>");
                println!("Running after_mount action: `{}`", action.after_mount());
                let result = self.execute_action(action.after_mount());
                println!("<anylinuxfs-force-output:off>");
                result?;
            }
        }
        Ok(())
    }

    pub fn before_unmount(&self) -> anyhow::Result<()> {
        if let Some(action) = &self.config {
            if !action.before_unmount().is_empty() {
                println!(
                    "Running before_unmount action: `{}`",
                    action.before_unmount()
                );
                self.execute_action(action.before_unmount())?;
            }
        }
        Ok(())
    }
}

// TODO: we might need this for custom actions on FreeBSD
#[cfg(target_os = "linux")]
fn statfs(path: impl AsRef<Path>) -> io::Result<libc::statfs> {
    let c_path = CString::from_path(path.as_ref());
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut buf) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buf)
}

fn export_args_for_path(
    _path: &str,
    export_mode: &str,
    _fsid: usize,
    export_args_override: Option<&str>,
) -> anyhow::Result<String> {
    #[cfg(target_os = "linux")]
    let mut export_args = if let Some(override_args) = export_args_override {
        override_args.to_owned()
    } else {
        format!("{export_mode},no_subtree_check,no_root_squash,insecure")
    };
    #[cfg(any(target_os = "freebsd", target_os = "macos"))]
    let export_args = if let Some(override_args) = export_args_override {
        override_args.to_owned()
    } else {
        format!(
            "{}-maproot=root",
            if export_mode == "ro" { "-ro " } else { "" }
        )
    };

    #[cfg(target_os = "linux")]
    if statfs(_path)
        .with_context(|| format!("statfs failed for {_path}"))?
        .f_type
        == 0x65735546
    {
        // exporting FUSE requires fsid
        if !export_args.contains("fsid=") {
            export_args += &format!(",fsid={}", _fsid)
        }
    }
    Ok(export_args)
}

const ALFS_PASSPHRASE_PREFIX: &[u8] = b"ALFS_PASSPHRASE";

fn get_pwds_from_env() -> HashMap<usize, BString> {
    let mut pwds = HashMap::new();
    for (key, value) in env::vars_os() {
        let key_bstr = BString::from(key.as_bytes());
        if key_bstr.starts_with(ALFS_PASSPHRASE_PREFIX) {
            let idx = key_bstr
                .strip_prefix(ALFS_PASSPHRASE_PREFIX)
                .and_then(|s| str::from_utf8(s).ok())
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1);
            let pwd = BString::from(value.as_bytes());
            pwds.insert(idx, pwd);
        }
    }
    pwds
}

#[cfg(any(target_os = "freebsd", target_os = "macos"))]
const KEY_FILE_MOUNT_DIR: &str = "/tmp/alfs_keyfiles";
#[cfg(any(target_os = "freebsd", target_os = "macos"))]
const KEY_FILE_ISO_DEV: &str = "/dev/iso9660/ALFS_KEYFILE";
#[cfg(any(target_os = "freebsd", target_os = "macos"))]
const KEY_FILE_NAME_IN_ISO: &str = "keyfile";

/// Resolve the key file path inside the VM.
/// For Linux: the path is directly accessible (passed via --key-file).
/// For FreeBSD: if the key file ISO device exists (identified by label ALFS_KEYFILE),
/// mount it and return the path to the key file inside it.
#[allow(unused_variables)]
fn setup_key_file_path(
    key_file: Option<String>,
    deferred: &mut Deferred,
) -> anyhow::Result<Option<String>> {
    #[cfg(any(target_os = "freebsd", target_os = "macos"))]
    if Path::new(KEY_FILE_ISO_DEV).exists() {
        fs::create_dir_all(KEY_FILE_MOUNT_DIR).context("Failed to create key file mount dir")?;
        let status = Command::new("mount")
            .args(["-t", "cd9660", KEY_FILE_ISO_DEV, KEY_FILE_MOUNT_DIR])
            .status()
            .context("Failed to mount key file ISO")?;
        if !status.success() {
            return Err(anyhow!(
                "Failed to mount key file ISO {}: exit code {}",
                KEY_FILE_ISO_DEV,
                status.code().unwrap_or(-1)
            ));
        }
        deferred.add(|| {
            let _ = Command::new("umount").arg(KEY_FILE_MOUNT_DIR).status();
        });
        return Ok(Some(format!(
            "{}/{}",
            KEY_FILE_MOUNT_DIR, KEY_FILE_NAME_IN_ISO
        )));
    }

    Ok(key_file)
}

fn main() -> ExitCode {
    if let Err(e) = run() {
        eprintln!("Error: {:#}", e);
        eprintln!("<anylinuxfs-exit-code:1>");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn mount_tmpfs(paths: &[&str]) -> anyhow::Result<()> {
    for path in paths {
        let status = script(&format!("mount -t tmpfs tmpfs {}", path))
            .status()
            .context(format!("Failed to mount tmpfs on {path}"))?;

        if !status.success() {
            return Err(anyhow!("Failed to mount tmpfs on {path}"));
        }
    }
    Ok(())
}

const KERNEL_LOG_PATH: &str = "/tmp/kernel.log";

/// Bundles the mutable disk/volume state that flows through the entire
/// vmproxy lifecycle — decryption, volume activation, filesystem detection,
/// mount-label resolution, mounting, and NFS export generation.
struct VmDiskContext {
    disk_path: String,
    fs_type: Option<String>,
    fs_driver: Option<String>,
    mount_options: Option<String>,
    mount_name: String,
    verbose: bool,
    mapper_ident_prefix: &'static str,
    cryptsetup_op: &'static str,
    assemble_raid: bool,
    env_pwds: HashMap<usize, BString>,
    key_file_path: Option<String>,
    // Derived state (populated during the lifecycle)
    is_raid: bool,
    is_zfs: bool,
    zfs_mountpoints: Vec<zfs::Mountpoint>,
    zfs_pools: Vec<String>,
}

impl VmDiskContext {
    fn new(cli: &Cli, key_file_path: Option<String>) -> Self {
        let (mapper_ident_prefix, cryptsetup_op) = match cli.fs_type.as_deref() {
            Some("crypto_LUKS") => ("luks", "open"),
            Some("BitLocker") => ("btlk", "bitlkOpen"),
            _ => ("luks", "open"),
        };

        VmDiskContext {
            disk_path: cli.disk_path.clone(),
            fs_type: cli.fs_type.clone(),
            fs_driver: cli.fs_driver.clone(),
            mount_options: cli.mount_options.clone(),
            mount_name: cli.mount_name.clone(),
            verbose: cli.verbose,
            mapper_ident_prefix,
            cryptsetup_op,
            assemble_raid: cli.assemble_raid,
            env_pwds: get_pwds_from_env(),
            key_file_path,
            is_raid: false,
            is_zfs: false,
            zfs_mountpoints: vec![],
            zfs_pools: vec![],
        }
    }

    fn env_has_passphrase(&self) -> bool {
        !self.env_pwds.is_empty()
    }

    fn is_logical(&self) -> bool {
        self.disk_path.starts_with("/dev/mapper") || self.is_raid
    }

    fn specified_read_only(&self) -> bool {
        self.mount_options
            .as_deref()
            .map(|opts| is_read_only_set(opts.split(',')))
            .unwrap_or(false)
    }

    /// Decrypt LUKS/BitLocker volumes using cryptsetup.
    fn decrypt(
        &self,
        decrypt_devs: &str,
        reuse_passphrase: bool,
        deferred: &mut Deferred,
    ) -> anyhow::Result<()> {
        let env_has_passphrase = self.env_has_passphrase();
        let (pwd_for_all, input_mode_fn): (_, fn() -> _) = if reuse_passphrase
            && self.key_file_path.is_none()
        {
            let pwd = if let Some(passphrase) = self.env_pwds.get(&1) {
                BString::from(passphrase.as_bytes())
            } else if env_has_passphrase {
                return Err(anyhow!(
                    "Missing environment variable {}",
                    ALFS_PASSPHRASE_PREFIX.as_bstr()
                ));
            } else {
                println!("<anylinuxfs-passphrase-prompt:start>");
                let prompt_end = deferred.add(|| println!("<anylinuxfs-passphrase-prompt:end>"));
                let pwd = BString::from(rpassword::read_password()?.as_bytes());
                deferred.call_now(prompt_end);
                pwd
            };
            (Some(pwd), || Stdio::piped())
        } else if env_has_passphrase && self.key_file_path.is_none() {
            (None, || Stdio::piped())
        } else {
            (None, || Stdio::inherit())
        };

        let key_file_args: &[&str] = if let Some(key_file) = self.key_file_path.as_deref() {
            &["--key-file", key_file]
        } else {
            &[]
        };
        for (i, dev) in decrypt_devs.split(",").enumerate() {
            let mut cryptsetup = Command::new("/sbin/cryptsetup")
                .arg("-T1")
                .arg(self.cryptsetup_op)
                .args(key_file_args)
                .arg(&dev)
                .arg(format!("{}{i}", self.mapper_ident_prefix))
                .stdin(input_mode_fn())
                .spawn()?;

            let pwd = pwd_for_all.as_ref().or(self.env_pwds.get(&(i + 1)));
            let cryptsetup_result = if let Some(pwd) = pwd {
                {
                    let mut stdin = cryptsetup.stdin.take().unwrap();
                    stdin.write_all(pwd.as_bytes())?;
                } // must close stdin before waiting for child
                cryptsetup.wait()?
            } else if self.key_file_path.is_some() {
                cryptsetup.wait()?
            } else if env_has_passphrase {
                return Err(anyhow!(
                    "Missing environment variable {}{} for device {}",
                    ALFS_PASSPHRASE_PREFIX.as_bstr(),
                    i + 1,
                    dev
                ));
            } else {
                println!("<anylinuxfs-passphrase-prompt:start>");
                let prompt_end = deferred.add(|| println!("<anylinuxfs-passphrase-prompt:end>"));
                let res = cryptsetup.wait()?;
                deferred.call_now(prompt_end);
                res
            };

            if !cryptsetup_result.success() {
                return Err(anyhow!(
                    "Failed to open encrypted device '{}': {}",
                    dev,
                    cryptsetup_result
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or("unknown".to_owned())
                ));
            }
        }
        Ok(())
    }

    /// Activate RAID and LVM volumes, updating `disk_path` and `fs_type`.
    fn activate_volume_managers(&mut self) -> anyhow::Result<()> {
        self.is_raid = self.assemble_raid || self.disk_path.starts_with("/dev/md");
        if self.is_raid {
            let _mdadm_assemble_result = Command::new("/sbin/mdadm")
                .arg("--assemble")
                .arg("--scan")
                .status()
                .context("Failed to run mdadm command")?;

            let md_path = script_output("mdadm --detail --scan | cut -d' ' -f2")
                .context("Failed to get RAID device path from mdadm")?
                .trim()
                .to_owned();

            if !md_path.is_empty() && !self.disk_path.starts_with("/dev/mapper") {
                self.disk_path = md_path;
            }
        }

        #[cfg(target_os = "linux")]
        let _vgchange_result = Command::new("/sbin/vgchange")
            .arg("-ay")
            .status()
            .context("Failed to run vgchange command")?;

        match self.fs_type.as_deref() {
            Some("crypto_LUKS") | Some("BitLocker") => {
                self.disk_path = format!("/dev/mapper/{}0", self.mapper_ident_prefix);
                self.fs_type = None;
            }
            _ => {}
        }

        self.is_zfs = self.fs_type.as_deref() == Some("zfs_member");
        Ok(())
    }

    /// Detect filesystem type from the disk using blkid.
    fn detect_fs_type(&mut self) -> anyhow::Result<()> {
        if self.disk_path.is_empty() {
            return Ok(());
        }

        match self.fs_type.as_deref() {
            Some("auto") | None => {
                let fs = Command::new("/sbin/blkid")
                    .arg(&self.disk_path)
                    .arg("-s")
                    .arg("TYPE")
                    .arg("-o")
                    .arg("value")
                    .output()
                    .context("Failed to run blkid command")?
                    .stdout;

                let fs = String::from_utf8_lossy(&fs).trim().to_owned();
                println!("<anylinuxfs-type:{}>", &fs);
                self.fs_type = if !fs.is_empty() { Some(fs) } else { None };
            }
            Some("zfs_member") => {
                self.fs_type = Some("zfs".to_owned());
                println!("<anylinuxfs-type:{}>", self.fs_type.as_deref().unwrap());
            }
            _ => (),
        }
        Ok(())
    }

    /// Resolve a filesystem label for the mount point name.
    fn resolve_mount_label(&mut self) -> anyhow::Result<()> {
        if !self.is_logical() {
            if self.is_zfs {
                #[cfg(target_os = "linux")]
                script("modprobe zfs")
                    .status()
                    .context("Failed to load zfs module")?;
                let label = "zfs_root".to_owned();
                println!("<anylinuxfs-label:{}>", &label);
                self.mount_name = label;
            }
        } else {
            let label = Command::new("/sbin/blkid")
                .arg(&self.disk_path)
                .arg("-s")
                .arg("LABEL")
                .arg("-o")
                .arg("value")
                .output()
                .context("Failed to run blkid command")?
                .stdout;

            if let Some(label) =
                path_safe_label_name(&String::from_utf8_lossy(&label).trim().to_owned())
            {
                println!("<anylinuxfs-label:{}>", &label);
                self.mount_name = label;
            }
        }
        Ok(())
    }

    /// Import ZFS pools and populate zfs_mountpoints / zfs_pools.
    fn import_zfs_pools(&mut self, mount_point: &str) -> anyhow::Result<()> {
        if !self.is_zfs {
            return Ok(());
        }
        let (status, mountpoints, zpools) =
            zfs::import_all_zpools(mount_point, self.specified_read_only())?;
        if !status.success() {
            return Err(anyhow!(
                "Importing zpools failed with error code {}",
                status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or("unknown".to_owned())
            ));
        }
        self.zfs_mountpoints = mountpoints;
        self.zfs_pools = zpools;
        Ok(())
    }

    /// Mount the filesystem (ZFS or regular) and register deferred cleanup.
    fn mount(&self, mount_point: &str, deferred: &mut Deferred) -> anyhow::Result<()> {
        let mnt_args = if !self.is_zfs {
            let mnt_args = [
                "-t",
                self.fs_driver
                    .as_deref()
                    .or(self.fs_type.as_deref())
                    .unwrap_or("auto"),
                &self.disk_path,
                mount_point,
            ]
            .into_iter()
            .chain(
                self.mount_options
                    .as_deref()
                    .into_iter()
                    .flat_map(|opts| ["-o", opts]),
            )
            .chain(self.verbose.then_some("-v").into_iter());

            let mnt_args: Vec<&str> = mnt_args.collect();
            println!("mount args: {:?}", &mnt_args);
            mnt_args
        } else {
            vec![]
        };

        // we must show any output of mount command
        // in case there's a warning (e.g. NTFS cannot be accessed rw)
        println!("<anylinuxfs-force-output:on>");
        let force_output_off = deferred.add(|| {
            println!("<anylinuxfs-force-output:off>");
        });

        let mnt_result = if self.is_zfs {
            zfs::mount_datasets(
                &self.zfs_mountpoints,
                &self.env_pwds,
                self.key_file_path.as_deref(),
            )?
        } else {
            let mount_bin = if cfg!(target_os = "freebsd") {
                "/sbin/mount"
            } else {
                "/bin/mount"
            };
            Command::new(mount_bin)
                .args(mnt_args)
                .status()
                .context("Failed to run mount command")?
        };

        if !mnt_result.success() {
            return Err(anyhow!(
                "Mounting {} on {} failed with error code {}",
                self.disk_path,
                mount_point,
                mnt_result
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or("unknown".to_owned())
            ));
        }
        deferred.call_now(force_output_off);

        println!(
            "'{}' mounted successfully on '{}', filesystem {}.",
            self.disk_path,
            mount_point,
            self.fs_type.as_deref().unwrap_or("unknown")
        );

        let is_zfs = self.is_zfs;
        let zfs_export_script = self
            .zfs_pools
            .iter()
            .rev()
            .map(|pool| format!("zpool export {}", pool))
            .collect::<Vec<String>>()
            .join(" && ");

        deferred.add({
            let mount_point = mount_point.to_owned();
            move || {
                let mut backoff = Duration::from_millis(50);
                let umount_action: &dyn Fn() -> _ = if is_zfs {
                    &|| script(&zfs_export_script).status().map(|_| ())
                } else {
                    #[cfg(target_os = "linux")]
                    {
                        &|| unmount(&mount_point, UnmountFlags::empty())
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        &|| Ok(())
                    }
                };
                while let Err(e) = umount_action() {
                    eprintln!("Failed to unmount '{}': {}", &mount_point, e);
                    thread::sleep(backoff);
                    backoff = std::cmp::min(backoff * 2, Duration::from_secs(32));
                }
                println!("Unmounted '{}' successfully.", &mount_point);

                _ = fs::remove_dir(&mount_point);
            }
        });

        Ok(())
    }

    /// Build NFS export configuration and write /etc/exports (or /tmp/exports).
    fn build_nfs_exports(
        &self,
        export_path: String,
        export_mode: &str,
        effective_export_args_override: Option<&str>,
    ) -> anyhow::Result<()> {
        let all_exports = if self.is_zfs {
            let mut paths: BTreeSet<_> = self
                .zfs_mountpoints
                .iter()
                .map(|m| m.path.clone())
                .collect();

            if !paths.contains(&export_path) {
                paths.insert(export_path);
            }

            let mut exports = vec![];
            for (i, p) in paths.into_iter().enumerate() {
                let a = export_args_for_path(&p, export_mode, i, effective_export_args_override)?;
                exports.push((p, a));
            }
            exports
        } else {
            let export_args =
                export_args_for_path(&export_path, export_mode, 0, effective_export_args_override)?;
            vec![(export_path, export_args)]
        };
        let mut exports_content = String::new();

        for (export_path, export_args) in &all_exports {
            println!("<anylinuxfs-nfs-export:{}>", export_path);
            #[cfg(target_os = "linux")]
            {
                exports_content += &format!("\"{}\"      *({})\n", export_path, export_args);
            }
            #[cfg(any(target_os = "freebsd", target_os = "macos"))]
            {
                exports_content += &format!("{} {},network 0.0.0.0/0\n", export_path, export_args);
            }
        }

        let nfs_exports_path = if cfg!(target_os = "freebsd") {
            "/etc/exports"
        } else {
            "/tmp/exports"
        };

        fs::write(nfs_exports_path, exports_content)
            .context(format!("Failed to write to {}", nfs_exports_path))?;
        println!("Successfully initialized {}.", nfs_exports_path);
        Ok(())
    }
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    #[cfg(target_os = "linux")]
    let tmpfs_dirs = &[
        "/.config",
        "/.cache",
        "/tmp",
        "/run",
        "/var/lib/nfs",
        "/etc/lvm/archive",
        "/etc/lvm/backup",
    ];

    #[cfg(any(target_os = "freebsd", target_os = "macos"))]
    let tmpfs_dirs = &["/tmp"];

    mount_tmpfs(tmpfs_dirs)?;

    #[cfg(target_os = "linux")]
    for dir in ["/var/lib/nfs/rpc_pipefs", "/var/lib/nfs/sm"] {
        fs::create_dir_all(dir).with_context(|| format!("Failed to create directory '{}'", dir))?;
    }

    init_network(&cli.bind_addrs, cli.host_rpcbind, cli.native_network)
        .context("Failed to initialize network")?;

    #[cfg(target_os = "linux")]
    let listener = {
        let addr = VsockAddr::new(VMADDR_CID_ANY, 12700);
        VsockListener::bind(&addr)?
    };
    #[cfg(any(target_os = "freebsd", target_os = "macos"))]
    let listener = TcpListener::bind(&format!("0.0.0.0:{}", VM_CTRL_PORT))?;

    let ctrl_server = CtrlSocketServer::new(listener);
    println!("<anylinuxfs-vmproxy-ready>");

    let mut deferred = Deferred::new();

    deferred.add(|| {
        let kernel_log_warning = format!(
            "Warning: failed to dump dmesg output to {}",
            KERNEL_LOG_PATH
        );
        match script(&format!("dmesg > {}", KERNEL_LOG_PATH)).status() {
            Ok(status) if !status.success() => {
                eprintln!("{}", kernel_log_warning);
            }
            Err(e) => {
                eprintln!("{}: {:#}", kernel_log_warning, e);
            }
            _ => {}
        }

        let mut kernel_log_content = Vec::new();
        match fs::File::open(KERNEL_LOG_PATH) {
            Ok(mut kernel_log_file) => _ = kernel_log_file.read_to_end(&mut kernel_log_content),
            Err(_) => {}
        }
        // we must move the log somewhere persistent where the host can access it;
        // guests such as FreeBSD might not be running from a virtiofs mounted root
        ctrl_server
            .send_report(vmctrl::Report {
                kernel_log: kernel_log_content.into(),
            })
            .ok();
    });

    let custom_action_cfg = if let Some(action) = cli.action.as_deref() {
        Some(CustomActionConfig::percent_decode(action)?)
    } else {
        None
    };
    let nfs_export_override = custom_action_cfg
        .as_ref()
        .map(|cfg| cfg.override_nfs_export().to_owned());
    let export_args_override = cli.nfs_export_opts.as_deref();
    let ignore_permissions = cli.ignore_permissions;
    let mut custom_action = CustomActionRunner::new(custom_action_cfg);

    // Resolve key file path inside the VM.
    // For Linux: the path is directly accessible via the virtiofs rootfs (--key-file arg).
    // For FreeBSD: detect the ISO by label and mount it automatically.
    let key_file_path = setup_key_file_path(cli.key_file.clone(), &mut deferred)
        .context("Failed to set up encryption key file")?;

    let mut dsk = VmDiskContext::new(&cli, key_file_path);

    // decrypt LUKS/BitLocker volumes if any
    if let Some(decrypt) = &cli.decrypt {
        dsk.decrypt(decrypt, cli.reuse_passphrase, &mut deferred)?;
    }

    dsk.activate_volume_managers()?;

    if !cli.custom_mount_point {
        dsk.resolve_mount_label()?;
    }

    dsk.detect_fs_type()?;

    // scan multidisk volumes
    if cli.multi_device && dsk.fs_type.as_deref() == Some("btrfs") {
        Command::new("/sbin/btrfs")
            .args(["device", "scan"])
            .status()
            .context("Failed to run btrfs command")?;
    }

    #[cfg(target_os = "freebsd")]
    {
        setup_writable_dirs_for_nfsd().context("Failed to setup writable dirs for nfsd")?;
    }

    let mnt_tmp_status = script("mount -t tmpfs tmpfs /mnt")
        .status()
        .context("Failed to mount tmpfs on /mnt")?;

    if !mnt_tmp_status.success() {
        return Err(anyhow!("Failed to mount tmpfs on /mnt"));
    }

    common_utils::fail_for_known_nonmountable_types(dsk.fs_type.as_deref())?;

    let mount_point = if !dsk.mount_name.is_empty() {
        let mount_point = format!("/mnt/{}", dsk.mount_name);
        custom_action.set_env("ALFS_VM_MOUNT_POINT", mount_point.clone());

        fs::create_dir_all(&mount_point)
            .context(format!("Failed to create directory '{}'", &mount_point))?;
        println!("Directory '{}' created successfully.", &mount_point);
        mount_point
    } else {
        "".into()
    };

    dsk.import_zfs_pools(&mount_point)?;

    #[cfg(all(feature = "freebsd", target_os = "linux"))]
    {
        // Inform the user about ZFS crypto performance on Linux arm64 and suggest using FreeBSD
        if dsk.zfs_mountpoints.iter().any(|m| m.encrypted) {
            println!("<anylinuxfs-force-output:on>");
            println!("Warning: Using encrypted ZFS datasets on Linux with ARM64 hardware results");
            println!("in degraded performance due to GPL/CDDL license incompatibility.");
            println!("You can use a FreeBSD VM which is not affected by this issue.");
            println!(
                "Simply run `anylinuxfs config --zfs-os freebsd` to set it as default for ZFS."
            );
            println!("For more information, see https://github.com/openzfs/zfs/issues/12171");
            println!("<anylinuxfs-force-output:off>");
        }
    }

    custom_action
        .before_mount()
        .context("before_mount action")?;

    if !dsk.disk_path.is_empty() && !mount_point.is_empty() {
        dsk.mount(&mount_point, &mut deferred)?;
    }

    custom_action.after_mount().context("after_mount action")?;

    deferred.add(move || {
        if let Err(e) = custom_action.before_unmount() {
            eprintln!("before_unmount action: {:#}", e);
        };
    });

    let effective_mount_options = {
        let opts = script_output(&format!(
            "mount | grep {} | awk -F'(' '{{ print $2 }}' | tr -d ')'",
            &dsk.disk_path
        ))
        .with_context(|| format!("Failed to get mount options for {}", &dsk.disk_path))?
        .trim()
        .to_owned();
        println!("Effective mount options: {}", opts);
        opts
    }
    .split(',')
    .map(|s| s.to_owned())
    .collect::<Vec<String>>();

    let effective_read_only = if dsk.is_zfs {
        // we don't check effective ro flag for ZFS
        // (it's only useful for NTFS in hibernation anyway)
        dsk.specified_read_only()
    } else {
        is_read_only_set(effective_mount_options.iter().map(String::as_str))
    };

    if dsk.specified_read_only() != effective_read_only {
        println!("<anylinuxfs-mount:changed-to-ro>");
    }

    let export_path = match nfs_export_override {
        Some(path) if !path.is_empty() => path,
        _ => mount_point,
    };

    let export_mode = if effective_read_only { "ro" } else { "rw" };

    let squash_opts_storage;
    let effective_export_args_override = if ignore_permissions && export_args_override.is_none() {
        squash_opts_storage =
            format!("{export_mode},no_subtree_check,all_squash,anonuid=0,anongid=0,insecure");
        Some(squash_opts_storage.as_str())
    } else {
        export_args_override
    };

    dsk.build_nfs_exports(export_path, export_mode, effective_export_args_override)?;

    match Command::new("/usr/local/bin/entrypoint.sh").spawn() {
        Ok(mut hnd) => {
            ctrl_server.wait_for_quit_cmd();
            println!("Exiting...");

            if let Err(e) = terminate_child(&mut hnd, "entrypoint.sh") {
                eprintln!("{:#}", e);
            }
        }
        Err(e) => {
            eprintln!("Failed to start entrypoint.sh: {:#}", e);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_read_only_set() {
        assert!(is_read_only_set(["ro"].into_iter()));
        assert!(is_read_only_set(["rw", "ro"].into_iter()));
        assert!(is_read_only_set(["noatime", "ro", "nosuid"].into_iter()));
        assert!(!is_read_only_set(["rw"].into_iter()));
        assert!(!is_read_only_set(["noatime"].into_iter()));
        assert!(!is_read_only_set(std::iter::empty()));
    }

    #[test]
    fn test_vm_disk_context_specified_read_only() {
        let cli = Cli::parse_from(["vmproxy", "/dev/vda", "test"]);
        let dsk = VmDiskContext::new(&cli, None);
        assert!(!dsk.specified_read_only());

        let cli = Cli::parse_from(["vmproxy", "/dev/vda", "test", "-o", "ro,noatime"]);
        let dsk = VmDiskContext::new(&cli, None);
        assert!(dsk.specified_read_only());
    }

    #[test]
    fn test_vm_disk_context_mapper_prefix_luks() {
        let cli = Cli::parse_from(["vmproxy", "/dev/vda", "test", "-t", "crypto_LUKS"]);
        let dsk = VmDiskContext::new(&cli, None);
        assert_eq!(dsk.mapper_ident_prefix, "luks");
        assert_eq!(dsk.cryptsetup_op, "open");
    }

    #[test]
    fn test_vm_disk_context_mapper_prefix_bitlocker() {
        let cli = Cli::parse_from(["vmproxy", "/dev/vda", "test", "-t", "BitLocker"]);
        let dsk = VmDiskContext::new(&cli, None);
        assert_eq!(dsk.mapper_ident_prefix, "btlk");
        assert_eq!(dsk.cryptsetup_op, "bitlkOpen");
    }

    #[test]
    fn test_vm_disk_context_mapper_prefix_default() {
        let cli = Cli::parse_from(["vmproxy", "/dev/vda", "test", "-t", "ext4"]);
        let dsk = VmDiskContext::new(&cli, None);
        assert_eq!(dsk.mapper_ident_prefix, "luks");
        assert_eq!(dsk.cryptsetup_op, "open");
    }

    #[test]
    fn test_vm_disk_context_is_logical() {
        let cli = Cli::parse_from(["vmproxy", "/dev/mapper/luks0", "test"]);
        let dsk = VmDiskContext::new(&cli, None);
        assert!(dsk.is_logical());

        let cli = Cli::parse_from(["vmproxy", "/dev/vda", "test"]);
        let dsk = VmDiskContext::new(&cli, None);
        assert!(!dsk.is_logical());

        let cli = Cli::parse_from(["vmproxy", "/dev/vda", "test"]);
        let mut dsk = VmDiskContext::new(&cli, None);
        dsk.is_raid = true;
        assert!(dsk.is_logical());
    }

    #[test]
    fn test_vm_disk_context_env_has_passphrase() {
        let cli = Cli::parse_from(["vmproxy", "/dev/vda", "test"]);
        let dsk = VmDiskContext::new(&cli, None);
        // Without any ALFS_PASSPHRASE env vars set, env_pwds should be empty
        assert!(!dsk.env_has_passphrase());
    }
}
