use anyhow::{Context, anyhow};
use bstr::{BString, ByteSlice};
use clap::Parser;
#[cfg(target_os = "linux")]
use common_utils::FromPath;
#[cfg(target_os = "freebsd")]
use common_utils::VM_CTRL_PORT;
use common_utils::{CustomActionConfig, Deferred, VM_GATEWAY_IP, VM_IP, path_safe_label_name};
#[cfg(target_os = "linux")]
use libc::VMADDR_CID_ANY;
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};
use std::env;
#[cfg(target_os = "linux")]
use std::ffi::CString;
use std::ffi::OsStr;
use std::io::{self, BufRead, Read, Write};
#[cfg(target_os = "freebsd")]
use std::net::{TcpListener, TcpStream};
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::Duration;
use std::{fs, io::BufReader};
#[cfg(target_os = "linux")]
use sys_mount::{UnmountFlags, unmount};
#[cfg(target_os = "linux")]
use vsock::{VsockAddr, VsockListener, VsockStream};

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
    #[arg(short = 't', long = "types")]
    fs_type: Option<String>,
    #[arg(long = "fs-driver")]
    fs_driver: Option<String>,
    #[arg(short = 'o', long = "options")]
    mount_options: Option<String>,
    #[arg(short, long)]
    decrypt: Option<String>,
    #[arg(short, long)]
    action: Option<String>,
    #[arg(short, long, default_value = LOCALHOST)]
    bind_addr: String,
    #[arg(short, long)]
    multi_device: bool,
    #[arg(short, long)]
    reuse_passphrase: bool,
    #[arg(short, long)]
    host_rpcbind: bool,
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Serialize, Debug)]
struct PortDef<'a> {
    local: &'a str,
    remote: &'a str,
}

const LOCALHOST: &str = "127.0.0.1";

fn expose_port(client: &reqwest::blocking::Client, port_def: &PortDef) -> anyhow::Result<()> {
    client
        .post(&format!("http://{VM_GATEWAY_IP}/services/forwarder/expose"))
        .json(port_def)
        .send()
        .and_then(|res| res.error_for_status())
        .context(format!("Failed to expose port: {:?}", port_def))?;

    Ok(())
}

fn init_network(bind_addr: &str, host_rpcbind: bool) -> anyhow::Result<()> {
    // resolv.conf is already initialized and always the same on FreeBSD
    #[cfg(target_os = "linux")]
    fs::write("/etc/resolv.conf", format!("nameserver {VM_GATEWAY_IP}\n"))
        .context("Failed to write /etc/resolv.conf")?;

    #[cfg(target_os = "linux")]
    let script = format!(
        "ip addr add {VM_IP}/24 dev eth0 \
            && ip link set eth0 up \
            && ip route add default via {VM_GATEWAY_IP} dev eth0",
    );
    #[cfg(target_os = "freebsd")]
    let script = format!(
        "ifconfig vtnet0 inet {VM_IP}/24 \
            && route add default {VM_GATEWAY_IP} \
            && ifconfig lo0 up",
    );

    Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .status()
        .context("Failed to configure network interface")?;

    let mut bind_addr_list = vec![bind_addr];
    if bind_addr != LOCALHOST {
        bind_addr_list.push(LOCALHOST);
    }

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

    for addr in bind_addr_list {
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

trait ClonableStream: Read + Write {
    fn try_clone(&self) -> io::Result<impl ClonableStream + 'static>;
}

#[cfg(target_os = "linux")]
impl ClonableStream for VsockStream {
    fn try_clone(&self) -> io::Result<impl ClonableStream + 'static> {
        self.try_clone()
    }
}

#[cfg(target_os = "freebsd")]
impl ClonableStream for TcpStream {
    fn try_clone(&self) -> io::Result<impl ClonableStream + 'static> {
        self.try_clone()
    }
}

trait StreamListener {
    fn incoming(&self) -> impl Iterator<Item = io::Result<impl ClonableStream>>;
}

#[cfg(target_os = "linux")]
impl StreamListener for VsockListener {
    fn incoming(&self) -> impl Iterator<Item = io::Result<impl ClonableStream>> {
        self.incoming()
    }
}

#[cfg(target_os = "freebsd")]
impl StreamListener for TcpListener {
    fn incoming(&self) -> impl Iterator<Item = io::Result<impl ClonableStream>> {
        self.incoming()
    }
}

fn wait_for_quit_cmd(listener: impl StreamListener) -> anyhow::Result<()> {
    for stream in listener.incoming() {
        let mut stream = stream?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut cmd = String::new();
        if reader.read_line(&mut cmd).is_ok() {
            println!("Received command: '{}'", cmd.trim());
            if cmd == "quit\n" {
                println!("Exiting...");
                stream.write(b"ok\n")?;
                stream.flush()?;
                break;
            }
            stream.write(b"unknown\n")?;
            stream.flush()?;
        }
    }
    Ok(())
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

fn export_args_for_path(_path: &str, export_mode: &str, _fsid: usize) -> anyhow::Result<String> {
    #[cfg(target_os = "linux")]
    let mut export_args = format!("{export_mode},no_subtree_check,no_root_squash,insecure");
    #[cfg(target_os = "freebsd")]
    let export_args = format!(
        "{}-maproot=root",
        if export_mode == "ro" { "-ro " } else { "" }
    );

    #[cfg(target_os = "linux")]
    if statfs(_path)
        .with_context(|| format!("statfs failed for {_path}"))?
        .f_type
        == 0x65735546
    {
        // exporting FUSE requires fsid
        export_args += &format!(",fsid={}", _fsid)
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

fn main() -> ExitCode {
    if let Err(e) = run() {
        eprintln!("Error: {:#}", e);
        eprintln!("<anylinuxfs-exit-code:1>");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

const KERNEL_LOG_PATH: &str = "/var/log/kernel.log";

fn run() -> anyhow::Result<()> {
    // println!("vmproxy started");
    // println!("uid = {}", unsafe { libc::getuid() });
    // println!("gid = {}", unsafe { libc::getgid() });

    // let kernel_cfg = procfs::kernel_config()?;
    // println!("Kernel config");
    // for (key, value) in kernel_cfg {
    //     println!("{} = {:?}", key, value);
    // }

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
        // TODO: on FreeBSD, we must move the log somewhere persistent where the host can access it
    });

    let cli = Cli::parse();

    let custom_action_cfg = if let Some(action) = cli.action.as_deref() {
        Some(CustomActionConfig::percent_decode(action)?)
    } else {
        None
    };
    let nfs_export_override = custom_action_cfg
        .as_ref()
        .map(|cfg| cfg.override_nfs_export().to_owned());
    let mut custom_action = CustomActionRunner::new(custom_action_cfg);

    let mut disk_path = cli.disk_path;
    let mut fs_type = cli.fs_type;
    let fs_driver = cli.fs_driver;
    let mount_options = cli.mount_options;
    let verbose = cli.verbose;

    let specified_read_only = mount_options
        .as_deref()
        .map(|opts| is_read_only_set(opts.split(',')))
        .unwrap_or(false);

    let (mapper_ident_prefix, cryptsetup_op) = match fs_type.as_deref() {
        Some("crypto_LUKS") => ("luks", "open"),
        Some("BitLocker") => ("btlk", "bitlkOpen"),
        _ => ("luks", "open"),
    };

    let env_pwds = get_pwds_from_env();
    let env_has_passphrase = !env_pwds.is_empty();

    // decrypt LUKS/BitLocker volumes if any
    if let Some(decrypt) = &cli.decrypt {
        let (pwd_for_all, input_mode_fn): (_, fn() -> _) = if cli.reuse_passphrase {
            let pwd = if let Some(passphrase) = env_pwds.get(&1) {
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
        } else if env_has_passphrase {
            (None, || Stdio::piped())
        } else {
            (None, || Stdio::inherit())
        };

        for (i, dev) in decrypt.split(",").enumerate() {
            let mut cryptsetup = Command::new("/sbin/cryptsetup")
                .arg("-T1")
                .arg(cryptsetup_op)
                .arg(&dev)
                .arg(format!("{mapper_ident_prefix}{i}"))
                .stdin(input_mode_fn())
                .spawn()?;

            let pwd = pwd_for_all.as_ref().or(env_pwds.get(&(i + 1)));
            let cryptsetup_result = if let Some(pwd) = pwd {
                {
                    let mut stdin = cryptsetup.stdin.take().unwrap();
                    stdin.write_all(pwd.as_bytes())?;
                } // must close stdin before waiting for child
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
    }

    // activate RAID volumes if any
    let is_raid = disk_path.starts_with("/dev/md");
    if is_raid {
        let _mdadm_assemble_result = Command::new("/sbin/mdadm")
            .arg("--assemble")
            .arg("--scan")
            .status()
            .context("Failed to run mdadm command")?;

        let md_path = script_output("mdadm --detail --scan | cut -d' ' -f2")
            .context("Failed to get RAID device path from mdadm")?
            .trim()
            .to_owned();

        if !md_path.is_empty() {
            disk_path = md_path;
        }
    }

    // activate LVM volumes if any
    // vgchange can return non-zero but still partially succeed
    #[cfg(target_os = "linux")]
    let _vgchange_result = Command::new("/sbin/vgchange")
        .arg("-ay")
        .status()
        .context("Failed to run vgchange command")?;

    match fs_type.as_deref() {
        Some("crypto_LUKS") => {
            disk_path = "/dev/mapper/luks0".into();
            fs_type = None;
        }
        Some("BitLocker") => {
            disk_path = "/dev/mapper/btlk0".into();
            fs_type = None;
        }
        _ => {}
    }
    let is_logical = disk_path.starts_with("/dev/mapper") || is_raid;
    let is_zfs = fs_type.as_deref() == Some("zfs_member");

    let name = &cli.mount_name;
    let mount_name = if !is_logical {
        if is_zfs {
            #[cfg(target_os = "linux")]
            script("modprobe zfs")
                .status()
                .context("Failed to load zfs module")?;
            let label = "zfs_root".to_owned();
            println!("<anylinuxfs-label:{}>", &label);
            label
        } else {
            name.to_owned()
        }
    } else {
        let label = Command::new("/sbin/blkid")
            .arg(&disk_path)
            .arg("-s")
            .arg("LABEL")
            .arg("-o")
            .arg("value")
            .output()
            .context("Failed to run blkid command")?
            .stdout;

        let label = path_safe_label_name(&String::from_utf8_lossy(&label).trim().to_owned())
            .unwrap_or(name.to_owned());
        println!("<anylinuxfs-label:{}>", &label);
        label
    };

    match fs_type.as_deref() {
        Some("auto") | None => {
            let fs = Command::new("/sbin/blkid")
                .arg(&disk_path)
                .arg("-s")
                .arg("TYPE")
                .arg("-o")
                .arg("value")
                .output()
                .context("Failed to run blkid command")?
                .stdout;

            let fs = String::from_utf8_lossy(&fs).trim().to_owned();
            println!("<anylinuxfs-type:{}>", &fs);
            fs_type = if !fs.is_empty() { Some(fs) } else { None };
        }
        Some("zfs_member") => {
            fs_type = Some("zfs".to_owned());
            println!("<anylinuxfs-type:{}>", fs_type.as_deref().unwrap());
        }
        _ => (),
    }

    // scan multidisk volumes
    if cli.multi_device && fs_type.as_deref() == Some("btrfs") {
        Command::new("/sbin/btrfs")
            .args(["device", "scan"])
            .status()
            .context("Failed to run btrfs command")?;
    }

    #[cfg(target_os = "freebsd")]
    {
        setup_writable_dirs_for_nfsd().context("Failed to setup writable dirs for nfsd")?;

        let mnt_tmp_status = script("mount -t tmpfs tmpfs /mnt")
            .status()
            .context("Failed to mount tmpfs on /mnt")?;

        if !mnt_tmp_status.success() {
            return Err(anyhow!("Failed to mount tmpfs on /mnt"));
        }
    }

    let mount_point = format!("/mnt/{}", mount_name);
    custom_action.set_env("ALFS_VM_MOUNT_POINT", mount_point.clone());

    fs::create_dir_all(&mount_point)
        .context(format!("Failed to create directory '{}'", &mount_point))?;
    println!("Directory '{}' created successfully.", &mount_point);

    // let supported_fs =
    //     SupportedFilesystems::new().context("Failed to get supported filesystems")?;

    // for fs in supported_fs.dev_file_systems() {
    //     println!("Supported filesystem: {:?}", fs);
    // }

    // for fs in supported_fs.nodev_file_systems() {
    //     println!("Supported nodev filesystem: {:?}", fs);
    // }

    // let mounted = Mount::builder()
    //     .fstype(FilesystemType::from(&supported_fs))
    //     .flags(MountFlags::RDONLY)
    //     // .data(data)
    //     .mount("/dev/vda", &mount_point)
    //     .context(format!("Failed to mount '/dev/vda' on '{}'", &mount_point))?;

    let zfs_mountpoints = if is_zfs {
        let (status, mountpoints) = zfs::import_all_zpools(&mount_point, specified_read_only)?;
        if !status.success() {
            return Err(anyhow!(
                "Importing zpools failed with error code {}",
                status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or("unknown".to_owned())
            ));
        }
        mountpoints
    } else {
        vec![]
    };

    #[cfg(all(feature = "freebsd", target_os = "linux"))]
    {
        // Inform the user about ZFS crypto performance on Linux arm64 and suggest using FreeBSD
        if zfs_mountpoints.iter().any(|m| m.encrypted) {
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

    let mnt_args = if !is_zfs {
        let mnt_args = [
            "-t",
            fs_driver
                .as_deref()
                .or(fs_type.as_deref())
                .unwrap_or("auto"),
            &disk_path,
            &mount_point,
        ]
        .into_iter()
        .chain(
            mount_options
                .as_deref()
                .into_iter()
                .flat_map(|opts| ["-o", opts]),
        )
        .chain(verbose.then_some("-v").into_iter());

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

    let mnt_result = if is_zfs {
        zfs::mount_datasets(&zfs_mountpoints, &env_pwds)?
    } else {
        Command::new("/bin/mount")
            .args(mnt_args)
            .status()
            .context("Failed to run mount command")?
    };

    if !mnt_result.success() {
        return Err(anyhow!(
            "Mounting {} on {} failed with error code {}",
            &disk_path,
            &mount_point,
            mnt_result
                .code()
                .map(|c| c.to_string())
                .unwrap_or("unknown".to_owned())
        ));
    }
    deferred.call_now(force_output_off);

    println!(
        "'{}' mounted successfully on '{}', filesystem {}.",
        &disk_path,
        &mount_point,
        fs_type.unwrap_or("unknown".to_owned())
    );

    deferred.add({
        let mount_point = mount_point.clone();
        move || {
            let mut backoff = Duration::from_millis(50);
            let umount_action: &dyn Fn() -> _ = if is_zfs {
                &|| script("zpool export -a").status().map(|_| ())
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
                std::thread::sleep(backoff);
                backoff = std::cmp::min(backoff * 2, Duration::from_secs(32));
            }
            println!("Unmounted '{}' successfully.", &mount_point);

            _ = fs::remove_dir(&mount_point);
        }
    });

    custom_action.after_mount().context("after_mount action")?;

    deferred.add(move || {
        if let Err(e) = custom_action.before_unmount() {
            eprintln!("before_unmount action: {:#}", e);
        };
    });

    let effective_mount_options = {
        let opts = script_output(&format!(
            "mount | grep {} | awk -F'(' '{{ print $2 }}' | tr -d ')'",
            &disk_path
        ))
        .with_context(|| format!("Failed to get mount options for {}", &disk_path))?
        .trim()
        .to_owned();
        println!("Effective mount options: {}", opts);
        opts
    }
    .split(',')
    .map(|s| s.to_owned())
    .collect::<Vec<String>>();

    init_network(&cli.bind_addr, cli.host_rpcbind).context("Failed to initialize network")?;

    // list_dir(mount_point);

    let effective_read_only = if is_zfs {
        // we don't check effective ro flag for ZFS
        // (it's only useful for NTFS in hibernation anyway)
        specified_read_only
    } else {
        is_read_only_set(effective_mount_options.iter().map(String::as_str))
    };

    if specified_read_only != effective_read_only {
        println!("<anylinuxfs-mount:changed-to-ro>");
    }

    let export_path = match nfs_export_override {
        Some(path) if !path.is_empty() => path,
        _ => mount_point,
    };

    let export_mode = if effective_read_only { "ro" } else { "rw" };

    let all_exports = if is_zfs {
        let mut paths: BTreeSet<_> = zfs_mountpoints.into_iter().map(|m| m.path).collect();

        if !paths.contains(&export_path) {
            paths.insert(export_path);
        }

        let mut exports = vec![];
        for (i, p) in paths.into_iter().enumerate() {
            let a = export_args_for_path(&p, export_mode, i)?;
            exports.push((p, a));
        }
        exports
    } else {
        // single export
        let export_args = export_args_for_path(&export_path, export_mode, 0)?;
        vec![(export_path, export_args)]
    };
    let mut exports_content = String::new();

    for (export_path, export_args) in &all_exports {
        println!("<anylinuxfs-nfs-export:{}>", export_path);
        #[cfg(target_os = "linux")]
        {
            exports_content += &format!("\"{}\"      *({})\n", export_path, export_args);
        }
        #[cfg(target_os = "freebsd")]
        {
            exports_content += &format!("{} {},network 0.0.0.0/0\n", export_path, export_args);
        }
    }

    fs::write("/etc/exports", exports_content).context("Failed to write to /etc/exports")?;
    println!("Successfully initialized /etc/exports.");

    // let curl_result = Command::new("curl")
    //     .arg("ifconfig.co")
    //     .status()
    //     .context("Failed to execute curl to check internet connectivity")?;

    // if !curl_result.success() {
    //     return Err(anyhow!(
    //         "Curl command failed with error code {}",
    //         curl_result
    //             .code()
    //             .map(|c| c.to_string())
    //             .unwrap_or("unknown".to_owned())
    //     ));
    // }

    match Command::new("/usr/local/bin/entrypoint.sh")
        // .env("NFS_VERSION", "3")
        // .env("NFS_DISABLE_VERSION_3", "1")
        .spawn()
    {
        Ok(mut hnd) => {
            #[cfg(target_os = "linux")]
            let listener = {
                let addr = VsockAddr::new(VMADDR_CID_ANY, 12700);
                VsockListener::bind(&addr)?
            };
            #[cfg(target_os = "freebsd")]
            let listener = TcpListener::bind(&format!("0.0.0.0:{}", VM_CTRL_PORT))?;
            if let Err(e) = wait_for_quit_cmd(listener) {
                eprintln!("Error while waiting for quit command: {:#}", e);
            }

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
