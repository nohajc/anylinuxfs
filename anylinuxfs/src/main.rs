use anyhow::{Context, anyhow};
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use common_utils::{guest_println, host_eprintln, host_println, log};
use devinfo::DevInfo;
use nanoid::nanoid;
use nix::unistd::{Uid, User};
use objc2_core_foundation::{
    CFDictionary, CFDictionaryGetValueIfPresent, CFRetained, CFRunLoopGetCurrent, CFRunLoopRun,
    CFRunLoopStop, CFString, CFURL, CFURLGetString, kCFRunLoopDefaultMode,
};
use objc2_disk_arbitration::{
    DADisk, DADiskCopyDescription, DARegisterDiskDisappearedCallback, DASessionCreate,
    DASessionScheduleWithRunLoop, DAUnregisterCallback,
};
use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::ops::Deref;
use std::os::fd::FromRawFd;
use std::os::unix::fs::chown;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::ptr::{NonNull, null, null_mut};
use std::thread;
use std::time::Duration;
use std::{
    env,
    ffi::CString,
    fs::remove_file,
    io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use notify::{RecursiveMode, Watcher};
use std::sync::mpsc;
use url::Url;
use utils::{Deferred, OutputAction, StatusError, write_to_pipe};

mod api;
#[allow(unused)]
mod bindings;
mod devinfo;
mod utils;

const LOCK_FILE: &str = "/tmp/anylinuxfs.lock";

fn to_exit_code(status: i32) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        libc::WTERMSIG(status) + 128
    } else {
        1
    }
}

fn run() -> i32 {
    let mut app = AppRunner::default();

    if let Err(e) = app.run() {
        if let Some(status_error) = e.downcast_ref::<StatusError>() {
            return match app.is_child {
                true => status_error.status,
                false => to_exit_code(status_error.status),
            };
        }
        if let Some(clap_error) = e.downcast_ref::<clap::Error>() {
            clap_error.exit();
        }
        host_eprintln!("Error: {:#}", e);
        return 1;
    }
    return 0;
}

fn main() {
    let code = run();
    if code != 0 {
        std::process::exit(code);
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Config {
    root_path: PathBuf,
    config_file_path: PathBuf,
    log_file_path: PathBuf,
    init_rootfs_path: PathBuf,
    kernel_path: PathBuf,
    gvproxy_path: PathBuf,
    vsock_path: String,
    vfkit_sock_path: String,
    invoker_uid: libc::uid_t,
    invoker_gid: libc::gid_t,
    sudo_uid: Option<libc::uid_t>,
    sudo_gid: Option<libc::gid_t>,
    krun: KrunConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct KrunConfig {
    #[serde(default = "KrunConfig::default_log_level", rename = "log_level")]
    log_level_numeric: u32,
    #[serde(default = "KrunConfig::default_num_vcpus")]
    num_vcpus: u8,
    #[serde(default = "KrunConfig::default_ram_size")]
    ram_size_mib: u32,
}

impl KrunConfig {
    fn default_log_level() -> u32 {
        0
    }

    fn default_num_vcpus() -> u8 {
        1
    }

    fn default_ram_size() -> u32 {
        512
    }
}

impl Default for KrunConfig {
    fn default() -> Self {
        KrunConfig {
            log_level_numeric: 0,
            num_vcpus: 1,
            ram_size_mib: 512,
        }
    }
}

#[allow(unused)]
enum KrunLogLevel {
    Off = 0,
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

#[allow(unused)]
impl KrunConfig {
    fn log_level(&self) -> KrunLogLevel {
        match self.log_level_numeric {
            0 => KrunLogLevel::Off,
            1 => KrunLogLevel::Error,
            2 => KrunLogLevel::Warn,
            3 => KrunLogLevel::Info,
            4 => KrunLogLevel::Debug,
            5 => KrunLogLevel::Trace,
            _ => KrunLogLevel::Off,
        }
    }

    fn set_log_level(&mut self, level: KrunLogLevel) {
        self.log_level_numeric = level as u32;
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct MountConfig {
    disk_path: String,
    read_only: bool,
    mount_options: Option<String>,
    verbose: bool,
    common: Config,
}

fn rand_string(len: usize) -> String {
    nanoid!(
        len,
        &[
            '1', '2', '3', '4', '5', '6', '7', '8', '9', '0', 'a', 'b', 'c', 'd', 'e', 'f', 'g',
            'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x',
            'y', 'z', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O',
            'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z',
        ]
    )
}

#[derive(Parser)]
#[command(version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    commands: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Mount a filesystem (the default if no command given)
    Mount(MountCmd),
    /// Init Linux rootfs (can be used to reinitialize virtual environment)
    Init,
    /// Show status information (mount parameters, vm resources, etc.)
    Status,
    /// Show log of current or previous run
    Log(LogCmd),
}

#[derive(Args)]
struct MountCmd {
    disk_path: String,
    #[arg(short, long)]
    options: Option<String>,
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Args)]
struct LogCmd {
    /// Wait for additional logs to be appended
    #[arg(short, long)]
    follow: bool,
}

#[derive(Parser)]
#[command(version, about = "Mount a filesystem (the default if no command given)", long_about = None)]
struct CliMount {
    #[command(flatten)]
    cmd: MountCmd,
}

trait TryParseCommand<T: FromArgMatches> {
    fn try_parse(self) -> Result<T, clap::Error>;
}

impl<T: FromArgMatches> TryParseCommand<T> for clap::Command {
    fn try_parse(self) -> Result<T, clap::Error> {
        self.try_get_matches().and_then(|m| T::from_arg_matches(&m))
    }
}

impl Cli {
    // try parse Cli; if it fails with InvalidSubcommand, try parse CliMount instead
    // (this effectively makes `mount` the default command so the keyword can be omitted)
    fn try_parse_with_default_cmd() -> Result<Cli, clap::Error> {
        let mount_cmd_usage = "\x1b[1manylinuxfs [mount]\x1b[0m [OPTIONS] <DISK_PATH>";
        let cmd = Cli::command().mut_subcommand("mount", |mount_cmd: clap::Command| {
            mount_cmd.override_usage(mount_cmd_usage)
        });

        cmd.try_parse().or_else(|err| match err.kind() {
            clap::error::ErrorKind::InvalidSubcommand => {
                let mount_cmd = CliMount::command().override_usage(mount_cmd_usage);
                let cli_mount: CliMount = mount_cmd.try_parse()?;
                Ok(Cli {
                    commands: Commands::Mount(cli_mount.cmd),
                })
            }
            _ => Err(err),
        })
    }
}

fn is_read_only_set(mount_options: Option<&str>) -> bool {
    if let Some(options) = mount_options {
        options.split(',').any(|opt| opt == "ro")
    } else {
        false
    }
}

fn load_config() -> anyhow::Result<Config> {
    let sudo_uid = env::var("SUDO_UID")
        .map_err(anyhow::Error::from)
        .and_then(|s| Ok(s.parse::<libc::uid_t>()?))
        .ok();
    // if let Some(sudo_uid) = sudo_uid {
    //     host_println!("sudo_uid = {}", sudo_uid);
    // }

    let sudo_gid = env::var("SUDO_GID")
        .map_err(anyhow::Error::from)
        .and_then(|s| Ok(s.parse::<libc::gid_t>()?))
        .ok();
    // if let Some(sudo_gid) = sudo_gid {
    //     host_println!("sudo_gid = {}", sudo_gid);
    // }

    let home_dir = homedir::my_home()
        .context("Failed to get home directory")?
        .context("Home directory not found")?;

    let uid = unsafe { libc::getuid() };
    if uid == 0 && (sudo_uid.is_none() || sudo_gid.is_none() || !home_dir.starts_with("/Users")) {
        eprintln!("This program must not be run directly by root but you can use sudo");
        std::process::exit(1);
    }
    let gid = unsafe { libc::getgid() };

    let invoker_uid = match sudo_uid {
        Some(sudo_uid) => sudo_uid,
        None => uid,
    };

    let invoker_gid = match sudo_gid {
        Some(sudo_gid) => sudo_gid,
        None => gid,
    };

    let exec_dir = env::current_exe()
        .context("Failed to get current executable path")?
        .parent()
        .context("Failed to get executable directory")?
        .to_owned();

    let prefix_dir = exec_dir
        .parent()
        .context("Failed to get prefix directory")?;

    // ~/.anylinuxfs/alpine/rootfs
    let root_path = home_dir.join(".anylinuxfs").join("alpine").join("rootfs");
    let config_file_path = home_dir.join(".anylinuxfs").join("config.toml");
    let log_file_path = home_dir.join("Library").join("Logs").join("anylinuxfs.log");

    let libexec_dir = prefix_dir.join("libexec");
    let init_rootfs_path = libexec_dir.join("init-rootfs").to_owned();
    let kernel_path = libexec_dir.join("Image").to_owned();
    let gvproxy_path = libexec_dir.join("gvproxy").to_owned();

    let vsock_path = format!("/tmp/anylinuxfs-{}-vsock", rand_string(8));
    let vfkit_sock_path = format!("/tmp/vfkit-{}.sock", rand_string(8));

    let krun = load_krun_config(&config_file_path)?;

    Ok(Config {
        root_path,
        config_file_path,
        log_file_path,
        init_rootfs_path,
        kernel_path,
        gvproxy_path,
        vsock_path,
        vfkit_sock_path,
        invoker_uid,
        invoker_gid,
        sudo_uid,
        sudo_gid,
        krun,
    })
}

fn load_krun_config(path: &Path) -> anyhow::Result<KrunConfig> {
    match fs::read_to_string(path) {
        Ok(config_str) => {
            let config: KrunConfig = toml::from_str(&config_str)
                .context(format!("Failed to parse config file {}", path.display()))?;
            Ok(config)
        }
        Err(_) => Ok(KrunConfig::default()),
    }
}

fn load_mount_config(cmd: MountCmd) -> anyhow::Result<MountConfig> {
    let (disk_path, mount_options) = if !cmd.disk_path.is_empty() {
        (cmd.disk_path, cmd.options)
    } else {
        host_eprintln!("No disk path provided");
        std::process::exit(1);
    };

    let read_only = is_read_only_set(mount_options.as_deref());
    let verbose = cmd.verbose;

    let common = load_config()?;

    Ok(MountConfig {
        disk_path,
        read_only,
        mount_options,
        verbose,
        common,
    })
}

fn drop_privileges(
    sudo_uid: Option<libc::uid_t>,
    sudo_gid: Option<libc::gid_t>,
) -> anyhow::Result<()> {
    if let (Some(sudo_uid), Some(sudo_gid)) = (sudo_uid, sudo_gid) {
        if unsafe { libc::setgid(sudo_gid) } < 0 {
            return Err(io::Error::last_os_error()).context("Failed to setgid");
        }
        if unsafe { libc::setuid(sudo_uid) } < 0 {
            return Err(io::Error::last_os_error()).context("Failed to setuid");
        }
    }
    Ok(())
}

fn setup_and_start_vm(
    config: &MountConfig,
    dev_info: &DevInfo,
    before_start: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let ctx = unsafe { bindings::krun_create_ctx() }.context("Failed to create context")?;

    let level = config.common.krun.log_level_numeric;
    unsafe { bindings::krun_set_log_level(level) }.context("Failed to set log level")?;

    let num_vcpus = config.common.krun.num_vcpus;
    let ram_mib = config.common.krun.ram_size_mib;
    unsafe { bindings::krun_set_vm_config(ctx, num_vcpus, ram_mib) }
        .context("Failed to set VM config")?;

    // run vmm as the original user if he used sudo
    if let Some(uid) = config.common.sudo_uid {
        unsafe { bindings::krun_setuid(ctx, uid) }.context("Failed to set vmm uid")?;
    }

    if let Some(gid) = config.common.sudo_gid {
        unsafe { bindings::krun_setgid(ctx, gid) }.context("Failed to set vmm gid")?;
    }

    unsafe { bindings::krun_set_root(ctx, CString::from_path(&config.common.root_path).as_ptr()) }
        .context("Failed to set root")?;

    unsafe {
        bindings::krun_add_disk(
            ctx,
            CString::new("data").unwrap().as_ptr(),
            CString::new(dev_info.rdisk()).unwrap().as_ptr(),
            config.read_only,
        )
    }
    .context("Failed to add disk")?;

    unsafe {
        bindings::krun_set_gvproxy_path(
            ctx,
            CString::new(config.common.vfkit_sock_path.as_str())
                .unwrap()
                .as_ptr(),
        )
    }
    .context("Failed to set gvproxy path")?;

    // let ports = vec![
    //     // CString::new("8000:8000").unwrap(),
    //     CString::new("111:111").unwrap(),
    //     CString::new("2049:2049").unwrap(),
    //     CString::new("32765:32765").unwrap(),
    //     CString::new("32767:32767").unwrap(),
    // ];
    // let port_map = ports
    //     .iter()
    //     .map(|s| s.as_ptr())
    //     .chain([std::ptr::null()])
    //     .collect::<Vec<_>>();

    // unsafe { bindings::krun_set_port_map(ctx, port_map.as_ptr()) }
    //     .context("Failed to set port map")?;

    vsock_cleanup(&config.common)?;

    unsafe {
        bindings::krun_add_vsock_port2(
            ctx,
            12700,
            CString::new(config.common.vsock_path.as_str())
                .unwrap()
                .as_ptr(),
            true,
        )
    }
    .context("Failed to add vsock port")?;

    unsafe { bindings::krun_set_workdir(ctx, CString::new("/").unwrap().as_ptr()) }
        .context("Failed to set workdir")?;

    let args: Vec<_> = [
        // CString::new("/bin/bash").unwrap(),
        // CString::new("-c").unwrap(),
        // CString::new("false").unwrap(),
        CString::new("/vmproxy").unwrap(),
        CString::new(dev_info.auto_mount_name()).unwrap(),
        CString::new("-t").unwrap(),
        CString::new(dev_info.fs_type().unwrap_or("auto")).unwrap(),
    ]
    .into_iter()
    .chain(
        config
            .mount_options
            .as_deref()
            .into_iter()
            .flat_map(|opts| [CString::new("-o").unwrap(), CString::new(opts).unwrap()]),
    )
    .chain(
        config
            .verbose
            .then_some(CString::new("-v").unwrap())
            .into_iter(),
    )
    .collect();

    // host_println!("vmproxy args: {:?}", &args);

    // let args = vec![CString::new("/bin/bash").unwrap()];
    let argv = args
        .iter()
        .map(|s| s.as_ptr())
        .chain([std::ptr::null()])
        .collect::<Vec<_>>();
    let envp = vec![std::ptr::null()];

    unsafe { bindings::krun_set_exec(ctx, argv[0], argv[1..].as_ptr(), envp.as_ptr()) }
        .context("Failed to set exec")?;

    unsafe {
        bindings::krun_set_kernel(
            ctx,
            CString::from_path(&config.common.kernel_path).as_ptr(),
            0, // KRUN_KERNEL_FORMAT_RAW
            null(),
            null(),
        )
    }
    .context("Failed to set kernel")?;

    before_start().context("Before start callback failed")?;
    unsafe { bindings::krun_start_enter(ctx) }.context("Failed to start VM")?;

    Ok(())
}

fn gvproxy_cleanup(config: &Config) -> anyhow::Result<()> {
    let sock_krun_path = config.vfkit_sock_path.replace(".sock", ".sock-krun.sock");
    match remove_file(&sock_krun_path) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("Failed to remove vfkit socket"),
    }
    match remove_file(&config.vfkit_sock_path) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("Failed to remove vfkit socket"),
    }
    Ok(())
}

fn vsock_cleanup(config: &Config) -> anyhow::Result<()> {
    match remove_file(&config.vsock_path) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("Failed to remove vsock socket"),
    }
    Ok(())
}

fn start_gvproxy(config: &Config) -> anyhow::Result<Child> {
    gvproxy_cleanup(config)?;

    let net_sock_uri = format!("unix:///tmp/network-{}.sock", rand_string(8));
    let vfkit_sock_uri = format!("unixgram://{}", &config.vfkit_sock_path);
    let gvproxy_args = ["--listen", &net_sock_uri, "--listen-vfkit", &vfkit_sock_uri];

    let mut gvproxy_cmd = Command::new(&config.gvproxy_path);

    gvproxy_cmd
        .args(&gvproxy_args)
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    if let (Some(uid), Some(gid)) = (config.sudo_uid, config.sudo_gid) {
        // run gvproxy with dropped privileges
        gvproxy_cmd.uid(uid).gid(gid);
    }

    let gvproxy_process = gvproxy_cmd
        .spawn()
        .context("Failed to start gvproxy process")?;

    Ok(gvproxy_process)
}

fn wait_for_port_while_child_running(port: u16, pid: libc::pid_t) -> anyhow::Result<bool> {
    let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    for _ in 0..50 {
        // Check if the child process is still running
        let mut status = 0;
        let res = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if res == -1 {
            return Err(io::Error::last_os_error()).context("Failed to wait for child process");
        } else if res > 0 {
            // Child process has exited
            host_eprintln!("VM process exited prematurely");
            return Ok(false);
        }

        match TcpStream::connect_timeout(&addr.into(), Duration::from_secs(10)) {
            Ok(_) => {
                return Ok(true);
            }
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                // Port is not open yet, continue waiting
            }
            Err(e) => {
                host_eprintln!("Error connecting to port {}: {}", port, e);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    Ok(false)
}

fn mount_nfs(share_name: &str) -> anyhow::Result<()> {
    let share_path = format!("/mnt/{share_name}");
    let apple_script = format!(
        "tell application \"Finder\" to open location \"nfs://localhost:{}\"",
        share_path
    );
    let status = Command::new("osascript")
        .arg("-e")
        .arg(apple_script)
        .status()?;

    if !status.success() {
        return Err(anyhow!(
            "osascript failed with exit code {}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or("unknown".to_owned())
        ));
    }
    Ok(())
}

// TODO: do we need this if umount can be used directly?
#[allow(unused)]
fn unmount_nfs(share_name: &str) -> anyhow::Result<()> {
    let volume_path = format!("/Volumes/{share_name}");
    let status = Command::new("umount").arg(&volume_path).status()?;

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

unsafe fn cfdict_get_value<'a, T>(dict: &'a CFDictionary, key: &str) -> Option<&'a T> {
    let key = CFString::from_str(key);
    let key_ptr: *const CFString = key.deref();
    let mut value_ptr: *const c_void = null();
    let key_found =
        unsafe { CFDictionaryGetValueIfPresent(dict, key_ptr as *const c_void, &mut value_ptr) };

    if !key_found {
        return None;
    }
    unsafe { (value_ptr as *const T).as_ref() }
}

struct DaDiskArgs {
    context: *mut c_void,
    descr: Option<CFRetained<CFDictionary>>,
}

impl DaDiskArgs {
    fn new(disk: NonNull<DADisk>, context: *mut c_void) -> Self {
        let descr = unsafe { DADiskCopyDescription(disk.as_ref()) };
        Self { context, descr }
    }

    fn mount_context(&self) -> &MountContext {
        unsafe { (self.context as *const MountContext).as_ref().unwrap() }
    }

    fn share_name(&self) -> &str {
        self.mount_context().share_name
    }

    fn descr(&self) -> Option<&CFDictionary> {
        self.descr.as_ref().map(|d| d.deref())
    }

    fn volume_path(&self) -> Option<String> {
        let volume_path: Option<&CFURL> =
            unsafe { cfdict_get_value(self.descr()?, "DAVolumePath") };
        volume_path
            .map(|url| unsafe { CFURLGetString(url).unwrap() }.to_string())
            .and_then(|url_str| Url::parse(&url_str).ok())
            .map(|url| url.path().to_string())
    }

    fn volume_kind(&self) -> Option<String> {
        let volume_kind: Option<&CFString> =
            unsafe { cfdict_get_value(self.descr()?, "DAVolumeKind") };
        volume_kind.map(|kind| kind.to_string())
    }
}

unsafe extern "C-unwind" fn disk_unmount_event(disk: NonNull<DADisk>, context: *mut c_void) {
    let args = DaDiskArgs::new(disk, context);

    if let (Some(volume_path), Some(volume_kind)) = (args.volume_path(), args.volume_kind()) {
        let expected_share_path = format!("/Volumes/{}/", args.share_name());
        if volume_kind == "nfs" && volume_path == expected_share_path {
            host_println!("Share {} was unmounted", &expected_share_path);
            unsafe { CFRunLoopStop(&CFRunLoopGetCurrent().unwrap()) };
        }
    }
}

// unsafe extern "C-unwind" fn disk_unmount_approval(
//     disk: NonNull<DADisk>,
//     context: *mut c_void,
// ) -> *const DADissenter {
//     let args = DaDiskArgs::new(disk, context);
//     if let Some(descr) = args.descr() {
//         inspect_cf_dictionary_values(descr);
//     }
//     if let (Some(volume_path), Some(volume_kind)) = (args.volume_path(), args.volume_kind()) {
//         let expected_share_path = format!("/Volumes/{}/", args.share_name());
//         if volume_kind == "nfs" && volume_path == expected_share_path {
//             host_println!("Approve unmount of {}? [y/n]", &expected_share_path);
//             let mut input = String::new();
//             io::stdin().read_line(&mut input).unwrap();
//             if input.trim() == "y" {
//                 return null();
//             }
//         }
//     }
//     let msg = CFString::from_str("custom error message");
//     let result = unsafe { DADissenterCreate(None, kDAReturnBusy, Some(&msg)) };
//     msg.retain();
//     result.retain();
//     result.deref()
// }

// fn inspect_cf_dictionary_values(dict: &CFDictionary) {
//     let count = unsafe { CFDictionaryGetCount(dict) } as usize;
//     let mut keys: Vec<*const c_void> = vec![null(); count];
//     let mut values: Vec<*const c_void> = vec![null(); count];

//     unsafe { CFDictionaryGetKeysAndValues(dict, keys.as_mut_ptr(), values.as_mut_ptr()) };

//     for i in 0..count {
//         let value = values[i] as *const CFType;
//         let type_id = unsafe { CFGetTypeID(value.as_ref()) };
//         let type_name = CFCopyTypeIDDescription(type_id).unwrap();
//         let key_str = keys[i] as *const CFString;

//         host_println!(
//             "Key: {}, Type: {}",
//             unsafe { key_str.as_ref().unwrap() },
//             &type_name,
//         );
//     }
// }

struct MountContext<'a> {
    share_name: &'a str,
}

fn wait_for_unmount(share_name: &str) -> anyhow::Result<()> {
    let session = unsafe { DASessionCreate(None).unwrap() };
    let mut mount_ctx = MountContext { share_name };
    let mount_ctx_ptr = &mut mount_ctx as *mut MountContext;
    unsafe {
        DARegisterDiskDisappearedCallback(
            &session,
            None,
            Some(disk_unmount_event),
            mount_ctx_ptr as *mut c_void,
        )
    };

    // unsafe {
    //     DARegisterDiskEjectApprovalCallback(
    //         &session,
    //         None,
    //         Some(disk_unmount_approval),
    //         mount_ctx_ptr as *mut c_void,
    //     )
    // }

    unsafe {
        DASessionScheduleWithRunLoop(
            &session,
            &CFRunLoopGetCurrent().unwrap(),
            kCFRunLoopDefaultMode.unwrap(),
        )
    };

    unsafe { CFRunLoopRun() };

    let callback_ptr = disk_unmount_event as *const c_void as *mut c_void;
    let callback_nonnull: NonNull<c_void> = NonNull::new(callback_ptr).unwrap();
    unsafe { DAUnregisterCallback(&session, callback_nonnull, null_mut()) };

    Ok(())
}

fn send_quit_cmd(config: &Config) -> anyhow::Result<()> {
    let mut stream = UnixStream::connect(&config.vsock_path)?;

    stream.write_all(b"quit\n")?;
    stream.flush()?;

    // we don't care about the response contents
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut buf = [0; 1024];
    _ = stream.read(&mut buf)?;

    Ok(())
}

fn terminate_child(child: &mut Child, child_name: &str) -> anyhow::Result<()> {
    common_utils::terminate_child(child, child_name, Some(log::Prefix::Host))
}

fn wait_for_file(file: impl AsRef<Path>) -> anyhow::Result<()> {
    let start = std::time::Instant::now();
    while !file.as_ref().exists() {
        if start.elapsed() > Duration::from_secs(5) {
            return Err(anyhow!(
                "Timeout waiting for file creation: {}",
                file.as_ref().to_string_lossy()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn init_rootfs(config: &Config, force: bool) -> anyhow::Result<()> {
    if !force {
        let bash_path = config.root_path.join("bin/bash");
        let nfsd_path = config.root_path.join("usr/sbin/rpc.nfsd");
        let entry_point_path = config.root_path.join("usr/local/bin/entrypoint.sh");
        let vmproxy_path = config.root_path.join("vmproxy");
        let required_files_exist = bash_path.exists()
            && nfsd_path.exists()
            && entry_point_path.exists()
            && vmproxy_path.exists();

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
        if required_files_exist && fstab_configured {
            // host_println!("VM root filesystem is initialized");
            return Ok(());
        }
    }

    host_println!("Initializing VM root filesystem...");

    let mut init_rootfs_cmd = Command::new(&config.init_rootfs_path);
    if let (Some(uid), Some(gid)) = (config.sudo_uid, config.sudo_gid) {
        // run init-rootfs with dropped privileges
        init_rootfs_cmd.uid(uid).gid(gid);
    }

    let status = init_rootfs_cmd
        .status()
        .context("Failed to execute init-rootfs")?;

    if !status.success() {
        return Err(anyhow!(
            "init-rootfs failed with exit code {}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or("unknown".to_owned())
        ));
    }

    Ok(())
}

struct AppRunner {
    is_child: bool,
}

impl Default for AppRunner {
    fn default() -> Self {
        Self { is_child: false }
    }
}

impl AppRunner {
    fn run_mount(&mut self, cmd: MountCmd) -> anyhow::Result<()> {
        let _lock_file = utils::acquire_flock(LOCK_FILE)?;
        let config = load_mount_config(cmd)?;
        let log_file_path = &config.common.log_file_path;

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

        let forked = utils::fork_with_comm_pipe()?;
        if forked.pid == 0 {
            self.is_child = true;
            let res = self.run_mount_child(config, forked.pipe_fd);
            if res.is_err() {
                unsafe { write_to_pipe(forked.pipe_fd, b"join\n") }
                    .context("Failed to write to pipe")?;
            }
            res
        } else {
            self.run_mount_parent(forked)
        }
    }

    fn run_mount_child(
        &mut self,
        config: MountConfig,
        comm_write_fd: libc::c_int,
    ) -> anyhow::Result<()> {
        let mut deferred = Deferred::new();

        init_rootfs(&config.common, false)?;

        if !config.verbose {
            log::disable_console_log();
        }

        // host_println!("disk_path: {}", config.disk_path);
        host_println!("root_path: {}", config.common.root_path.to_string_lossy());
        host_println!("num_vcpus: {}", config.common.krun.num_vcpus);
        host_println!("ram_size_mib: {}", config.common.krun.ram_size_mib);

        let dev_info = DevInfo::new(&config.disk_path)?;

        host_println!("disk: {}", dev_info.disk());
        host_println!("rdisk: {}", dev_info.rdisk());
        host_println!("label: {:?}", dev_info.label());
        host_println!("fs_type: {:?}", dev_info.fs_type());
        host_println!("uuid: {:?}", dev_info.uuid());
        host_println!("mount name: {}", dev_info.auto_mount_name());

        let mut gvproxy = start_gvproxy(&config.common)?;
        wait_for_file(&config.common.vfkit_sock_path)?;

        _ = deferred.add(|| {
            if let Err(e) = gvproxy_cleanup(&config.common) {
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
            if let Err(e) = terminate_child(&mut gvproxy, "gvproxy") {
                host_eprintln!("{:#}", e);
            }
        });

        _ = deferred.add(|| {
            if let Err(e) = vsock_cleanup(&config.common) {
                host_eprintln!("{:#}", e);
            }
        });

        let mut forked = utils::fork_with_pty_output(OutputAction::RedirectLater)?;
        if forked.pid == 0 {
            // Child process
            deferred.remove_all(); // deferred actions must be only called in the parent process

            setup_and_start_vm(&config, &dev_info, || forked.redirect())
                .context("Failed to start microVM")?;
        } else {
            // Parent process
            api::serve_info(&config, &dev_info);

            // Spawn a thread to read from the pipe
            _ = thread::spawn(move || {
                let mut buf_reader = BufReader::new(unsafe { File::from_raw_fd(forked.pipe_fd) });
                let mut line = String::new();
                while let Ok(bytes) = buf_reader.read_line(&mut line) {
                    if bytes == 0 {
                        break; // EOF
                    }
                    guest_println!("{}", line.trim_end());
                    line.clear();
                }
            });

            // drop privileges back to the original user if he used sudo
            drop_privileges(config.common.sudo_uid, config.common.sudo_gid)?;

            // TODO: Can we actually wait for the guest NFS server to be ready?
            // It seems the port is open as soon as port forwarding is configured.
            // This is needed if gvproxy port forwarding fails for example.
            let is_open = wait_for_port_while_child_running(111, forked.pid).unwrap_or(false);

            if is_open {
                host_println!("Port 111 is open");

                if unsafe { libc::setsid() } < 0 {
                    host_eprintln!("Failed to setsid, cannot run in the background");
                    // tell the parent to wait for the child to exit
                    unsafe { write_to_pipe(comm_write_fd, b"join\n") }
                        .context("Failed to write to pipe")?;
                } else {
                    // tell the parent to detach from console (i.e. exit)
                    unsafe { write_to_pipe(comm_write_fd, b"detach\n") }
                        .context("Failed to write to pipe")?;

                    // stop printing to the console
                    log::disable_console_log();
                }

                // mount nfs share
                let share_name = dev_info.auto_mount_name();
                match mount_nfs(&share_name) {
                    Ok(_) => host_println!("NFS share mounted successfully"),
                    Err(e) => host_eprintln!("Failed to mount NFS share: {:#}", e),
                }

                wait_for_unmount(share_name)?;
                send_quit_cmd(&config.common)?;
            } else {
                host_println!("Port 111 is not open");
                // tell the parent to wait for the child to exit
                unsafe { write_to_pipe(comm_write_fd, b"join\n") }
                    .context("Failed to write to pipe")?;
            }

            let mut status = 0;
            let wait_result = unsafe { libc::waitpid(forked.pid, &mut status, 0) };
            let last_error = io::Error::last_os_error();
            if wait_result < 0 && last_error.raw_os_error().unwrap() != libc::ECHILD {
                host_eprintln!("Failed to wait for child process: {}", last_error);
            }
            host_println!("libkrun VM exited with status: {}", status);

            if status != 0 {
                return Err(StatusError::new("VM exited with status", status).into());
            }
        }

        Ok(())
    }

    fn run_mount_parent(&mut self, forked: utils::ForkOutput) -> anyhow::Result<()> {
        let comm_read_fd = forked.pipe_fd;
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

    fn run_init(&mut self) -> anyhow::Result<()> {
        let _lock_file = utils::acquire_flock(LOCK_FILE)?;
        let config = load_config()?;
        init_rootfs(&config, true)?;

        Ok(())
    }

    fn run_log(&mut self, cmd: LogCmd) -> anyhow::Result<()> {
        let config = load_config()?;
        let log_file_path = &config.log_file_path;

        if !log_file_path.exists() {
            return Ok(());
        }

        let log_file = File::open(log_file_path).context("Failed to open log file")?;
        let mut buf_reader = BufReader::new(log_file);
        let mut line = String::new();

        // Print existing lines in the log file
        loop {
            let size = buf_reader.read_line(&mut line)?;
            if size == 0 {
                break; // EOF
            }
            println!("{}", line.trim_end());
            line.clear();
        }

        if cmd.follow {
            // Set up a file watcher to detect changes
            let (tx, rx) = mpsc::channel();
            let mut watcher = notify::recommended_watcher(tx)?;
            watcher
                .watch(log_file_path, RecursiveMode::NonRecursive)
                .context("Failed to watch log file")?;

            loop {
                match rx.recv() {
                    Ok(_) => {
                        // Read new lines appended to the file
                        while let Ok(size) = buf_reader.read_line(&mut line) {
                            if size == 0 {
                                break; // No more new lines
                            }
                            println!("{}", line.trim_end());
                            line.clear();
                        }
                    }
                    Err(e) => {
                        eprintln!("Watcher error: {}", e);
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    fn run_status(&mut self) -> anyhow::Result<()> {
        let resp = api::Client::make_request(api::Request::GetConfig);

        match resp {
            Ok(api::Response::Config(config)) => {
                let info: Vec<_> = config
                    .dev_info
                    .fs_type()
                    .into_iter()
                    .chain(
                        config
                            .mount_config
                            .mount_options
                            .iter()
                            .flat_map(|opts| opts.split(',')),
                    )
                    .collect();

                let user_name =
                    User::from_uid(Uid::from_raw(config.mount_config.common.invoker_uid))
                        .ok()
                        .flatten()
                        .map(|u| u.name)
                        .unwrap_or("<unknown>".into());

                println!(
                    "{} on /Volumes/{} ({}, mounted by {}) VM[cpus: {}, ram: {} MiB]",
                    &config.mount_config.disk_path,
                    config.dev_info.auto_mount_name(),
                    info.join(", "),
                    &user_name,
                    config.mount_config.common.krun.num_vcpus,
                    config.mount_config.common.krun.ram_size_mib,
                );
            }
            Err(err) => {
                if let Some(err) = err.downcast_ref::<io::Error>() {
                    match err.kind() {
                        io::ErrorKind::ConnectionRefused => return Ok(()),
                        _ => (),
                    }
                }
                return Err(err);
            }
        }

        Ok(())
    }

    fn run(&mut self) -> anyhow::Result<()> {
        // host_println!("uid = {}", unsafe { libc::getuid() });
        // host_println!("gid = {}", unsafe { libc::getgid() });

        let cli = Cli::try_parse_with_default_cmd()?;
        match cli.commands {
            Commands::Mount(cmd) => self.run_mount(cmd),
            Commands::Init => self.run_init(),
            Commands::Status => self.run_status(),
            Commands::Log(cmd) => self.run_log(cmd),
        }
    }
}

trait FromPath {
    fn from_path(path: impl AsRef<Path>) -> Self;
}

impl FromPath for CString {
    fn from_path(path: impl AsRef<Path>) -> Self {
        CString::new(path.as_ref().as_os_str().as_bytes()).unwrap()
    }
}

trait ResultWithCtx {
    type Value;
    fn context(self, msg: &str) -> anyhow::Result<Self::Value>;
}

impl ResultWithCtx for i32 {
    type Value = u32;
    fn context(self, msg: &str) -> anyhow::Result<Self::Value> {
        if self < 0 {
            Err(io::Error::from_raw_os_error(-self)).context(msg.to_owned())
        } else {
            Ok(self as u32)
        }
    }
}
