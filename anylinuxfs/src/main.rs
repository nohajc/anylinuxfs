use anyhow::{Context, anyhow};
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use common_utils::{
    guest_println, host_eprintln, host_println, log, prefix_eprintln, prefix_println, safe_println,
};
use devinfo::DevInfo;
use nanoid::nanoid;
use nix::unistd::{Uid, User};

use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::os::fd::FromRawFd;
use std::os::unix::fs::chown;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::ptr::null;
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
use std::sync::{Arc, Mutex, mpsc};
use utils::{AcquireLock, Deferred, FlockKind, LockFile, OutputAction, StatusError, write_to_pipe};

mod api;
#[allow(unused)]
mod bindings;
mod devinfo;
mod diskutil;
mod fsutil;
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

    let exit_code = if let Err(e) = app.run() {
        if let Some(status_error) = e.downcast_ref::<StatusError>() {
            match app.is_child {
                true => status_error.status,
                false => to_exit_code(status_error.status),
            }
        } else if let Some(clap_error) = e.downcast_ref::<clap::Error>() {
            clap_error.exit();
        } else {
            if let Some(print_error) = e.downcast_ref::<log::PrintError>() {
                if print_error.broken_pipe() {
                    return 1;
                }
            }
            host_eprintln!("Error: {:#}", e);
            1
        }
    } else {
        0
    };

    if app.print_log {
        log::print_log_file();
    }
    return exit_code;
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
    root_ver_file_path: PathBuf,
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

impl Display for KrunConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "log_level = {}\nnum_vcpus = {}\nram_size_mib = {}",
            self.log_level(),
            self.num_vcpus,
            self.ram_size_mib
        )
    }
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum KrunLogLevel {
    Off = 0,
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

impl Display for KrunLogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let val = match self {
            KrunLogLevel::Off => "off",
            KrunLogLevel::Error => "error",
            KrunLogLevel::Warn => "warn",
            KrunLogLevel::Info => "info",
            KrunLogLevel::Debug => "debug",
            KrunLogLevel::Trace => "trace",
        };
        write!(f, "{}", val)
    }
}

impl From<u32> for KrunLogLevel {
    fn from(value: u32) -> Self {
        match value {
            0 => KrunLogLevel::Off,
            1 => KrunLogLevel::Error,
            2 => KrunLogLevel::Warn,
            3 => KrunLogLevel::Info,
            4 => KrunLogLevel::Debug,
            5 => KrunLogLevel::Trace,
            _ => KrunLogLevel::Off,
        }
    }
}

#[allow(unused)]
impl KrunConfig {
    fn log_level(&self) -> KrunLogLevel {
        self.log_level_numeric.into()
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
    /// Show or change microVM parameters
    Config(ConfigCmd),
    /// List all available disks with Linux filesystems
    List,
    /// Stop anylinuxfs (can be used if unresponsive)
    Stop(StopCmd),
    /// microVM shell for debugging
    Shell(MountCmd),
}

#[derive(Args)]
struct MountCmd {
    /// Currently supports individual disk partitions
    disk_path: String,
    /// Options passed to the Linux mount command
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

#[derive(Args, Default, PartialEq, Eq)]
struct ConfigCmd {
    /// Set krun log level
    #[arg(short, long)]
    log_level: Option<KrunLogLevel>,
    /// Set number of vCPUs
    #[arg(short, long)]
    num_vcpus: Option<u8>,
    /// Set RAM size in MiB
    #[arg(short, long)]
    ram_size_mib: Option<u32>,
}

#[derive(Args)]
struct StopCmd {
    /// Force stop the VM
    #[arg(short, long)]
    force: bool,
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

    let exec_dir = fs::canonicalize(env::current_exe().context("Failed to get executable path")?)
        .context("Failed to get resolved exec path")?
        .parent()
        .context("Failed to get executable directory")?
        .to_owned();

    let prefix_dir = exec_dir
        .parent()
        .context("Failed to get prefix directory")?;

    // ~/.anylinuxfs/alpine/rootfs
    let alpine_path = home_dir.join(".anylinuxfs").join("alpine");
    let root_path = alpine_path.join("rootfs");
    let root_ver_file_path = alpine_path.join("rootfs.ver");
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
        root_ver_file_path,
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

fn save_krun_config(krun_config: &KrunConfig, config_file_path: &Path) -> anyhow::Result<()> {
    let config_str =
        toml::to_string(krun_config).context("Failed to serialize KrunConfig to TOML")?;
    fs::write(config_file_path, config_str).context(format!(
        "Failed to write config file {}",
        config_file_path.display()
    ))?;
    Ok(())
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

fn setup_vm(config: &MountConfig, dev_info: &DevInfo, use_gvproxy: bool) -> anyhow::Result<u32> {
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

    if use_gvproxy {
        unsafe {
            bindings::krun_set_gvproxy_path(
                ctx,
                CString::new(config.common.vfkit_sock_path.as_str())
                    .unwrap()
                    .as_ptr(),
            )
        }
        .context("Failed to set gvproxy path")?;
    }

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

    Ok(ctx)
}

fn start_vmproxy(
    ctx: u32,
    config: &MountConfig,
    dev_info: &DevInfo,
    before_start: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
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

    before_start().context("Before start callback failed")?;
    unsafe { bindings::krun_start_enter(ctx) }.context("Failed to start VM")?;

    Ok(())
}

fn start_vmshell(ctx: u32) -> anyhow::Result<()> {
    let args = vec![CString::new("/bin/bash").unwrap()];

    let argv = args
        .iter()
        .map(|s| s.as_ptr())
        .chain([std::ptr::null()])
        .collect::<Vec<_>>();
    let envp = vec![std::ptr::null()];

    unsafe { bindings::krun_set_exec(ctx, argv[0], argv[1..].as_ptr(), envp.as_ptr()) }
        .context("Failed to set exec")?;

    unsafe { bindings::krun_start_enter(ctx) }.context("Failed to start VM")?;

    Ok(())
}

struct VMOutput {
    status: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn read_all_from_fd(fd: i32) -> anyhow::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut reader = unsafe { File::from_raw_fd(fd) };
    let mut buf = [0; 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => output.extend_from_slice(&buf[..n]),
            Err(e) => {
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e).context("Failed to read from pipe");
            }
        }
    }
    Ok(output)
}

fn run_vmcommand(
    config: &MountConfig,
    dev_info: &DevInfo,
    use_gvproxy: bool,
    args: Vec<CString>,
) -> anyhow::Result<VMOutput> {
    let forked = utils::fork_with_piped_output()?;
    if forked.pid == 0 {
        // child process
        let ctx = setup_vm(config, dev_info, use_gvproxy)?;

        let argv = args
            .iter()
            .map(|s| s.as_ptr())
            .chain([std::ptr::null()])
            .collect::<Vec<_>>();
        let envp = vec![std::ptr::null()];

        unsafe { bindings::krun_set_exec(ctx, argv[0], argv[1..].as_ptr(), envp.as_ptr()) }
            .context("Failed to set exec")?;

        unsafe { bindings::krun_start_enter(ctx) }.context("Failed to start VM")?;
        unreachable!();
    } else {
        // parent process
        let stdout = read_all_from_fd(forked.out_fd)?;
        let stderr = match forked.err_fd {
            Some(fd) => read_all_from_fd(fd)?,
            None => Vec::new(),
        };

        let mut status = 0;
        if unsafe { libc::waitpid(forked.pid, &mut status, 0) } < 0 {
            return Err(io::Error::last_os_error()).context("Failed to wait for child process");
        }
        return Ok(VMOutput {
            status: to_exit_code(status),
            stdout,
            stderr,
        });
    }
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

enum NfsStatus {
    Ready,
    Failed(Option<i32>),
}

impl NfsStatus {
    fn ok(&self) -> bool {
        matches!(self, NfsStatus::Ready)
    }
}

fn wait_for_nfs_server(
    port: u16,
    nfs_notify_rx: mpsc::Receiver<NfsStatus>,
) -> anyhow::Result<NfsStatus> {
    // this will block until NFS server is ready or the VM exits
    let nfs_ready = nfs_notify_rx.recv()?;

    if nfs_ready.ok() {
        // also check if the port is open
        let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
        match TcpStream::connect_timeout(&addr.into(), Duration::from_secs(10)) {
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

fn unmount_nfs(volume_path: &Path) -> anyhow::Result<()> {
    let status = Command::new("umount").arg(volume_path).status()?;

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

fn send_quit_cmd(config: &Config) -> anyhow::Result<()> {
    let mut stream = UnixStream::connect(&config.vsock_path)?;

    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
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
                file.as_ref().display()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

const ROOTFS_CURRENT_VERSION: &str = "1.0.0";

fn rootfs_version_matches(config: &Config) -> bool {
    let root_ver_file_path = config.root_ver_file_path.as_path();
    let version = if root_ver_file_path.exists() {
        fs::read_to_string(root_ver_file_path)
            .unwrap_or_default()
            .trim()
            .to_string()
    } else {
        "".into()
    };
    if version != ROOTFS_CURRENT_VERSION {
        host_eprintln!("New version detected.");
        return false;
    }
    true
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
        if required_files_exist && fstab_configured && rootfs_version_matches(&config) {
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

    let mut hnd = init_rootfs_cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to execute init-rootfs")?;

    let out = BufReader::new(hnd.stdout.take().unwrap());
    let err = BufReader::new(hnd.stderr.take().unwrap());

    let thread = thread::spawn(move || {
        for line in err.lines() {
            if let Ok(line) = line {
                prefix_println!(None, "{}", line);
            }
        }
    });

    for line in out.lines() {
        if let Ok(line) = line {
            prefix_eprintln!(None, "{}", line);
        }
    }

    thread.join().unwrap();

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
    host_println!("libkrun VM exited with status: {}", status);
    Ok(Some(status))
}

// when the process isn't a child
fn wait_for_proc_exit(pid: libc::pid_t) -> anyhow::Result<()> {
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(5) {
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

enum MountStatus<'a> {
    NotYet,
    Mounted(&'a Path),
    NoLonger,
}

fn validated_mount_point(rt_info: &api::RuntimeInfo) -> MountStatus {
    let Some(mount_point) = rt_info.mount_point.as_ref().map(Path::new) else {
        return MountStatus::NotYet;
    };
    // let mount_point = Path::new("/Volumes").join("data");

    let expected_mount_dev = PathBuf::from(format!(
        "localhost:/mnt/{}",
        rt_info.dev_info.auto_mount_name()
    ));
    match fsutil::mounted_from(&mount_point) {
        Ok(mount_dev) if mount_dev == expected_mount_dev => MountStatus::Mounted(mount_point),
        _ => MountStatus::NoLonger,
    }
}

struct AppRunner {
    is_child: bool,
    print_log: bool,
}

impl Default for AppRunner {
    fn default() -> Self {
        Self {
            is_child: false,
            print_log: false,
        }
    }
}

impl AppRunner {
    fn run_shell(&mut self, cmd: MountCmd) -> anyhow::Result<()> {
        let _lock_file = LockFile::new(LOCK_FILE)?.acquire_lock(FlockKind::Exclusive)?;
        let config = load_mount_config(cmd)?;

        init_rootfs(&config.common, false)?;

        if !config.verbose {
            log::disable_console_log();
        }

        // host_println!("disk_path: {}", config.disk_path);
        host_println!("root_path: {}", config.common.root_path.display());
        host_println!("num_vcpus: {}", config.common.krun.num_vcpus);
        host_println!("ram_size_mib: {}", config.common.krun.ram_size_mib);

        if !Path::new(&config.disk_path).exists() {
            return Err(anyhow!("disk {} not found", &config.disk_path));
        }

        let dev_info = DevInfo::new(&config.disk_path)?;

        let _disk = File::open(dev_info.rdisk())?.acquire_lock(if config.read_only {
            FlockKind::Shared
        } else {
            FlockKind::Exclusive
        })?;

        host_println!("disk: {}", dev_info.disk());
        host_println!("rdisk: {}", dev_info.rdisk());
        host_println!("label: {:?}", dev_info.label());
        host_println!("fs_type: {:?}", dev_info.fs_type());
        host_println!("uuid: {:?}", dev_info.uuid());
        host_println!("mount name: {}", dev_info.auto_mount_name());

        let ctx = setup_vm(&config, &dev_info, false).context("Failed to setup microVM")?;
        start_vmshell(ctx).context("Failed to start microVM shell")?;

        Ok(())
    }

    fn run_mount(&mut self, cmd: MountCmd) -> anyhow::Result<()> {
        let _lock_file = LockFile::new(LOCK_FILE)?.acquire_lock(FlockKind::Exclusive)?;
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
            let verbose = config.verbose;
            let res = self.run_mount_child(config, forked.out_fd);
            if res.is_err() {
                if !verbose {
                    self.print_log = true;
                }
                unsafe { write_to_pipe(forked.out_fd, b"join\n") }
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
        host_println!("root_path: {}", config.common.root_path.display());
        host_println!("num_vcpus: {}", config.common.krun.num_vcpus);
        host_println!("ram_size_mib: {}", config.common.krun.ram_size_mib);

        if !Path::new(&config.disk_path).exists() {
            return Err(anyhow!("disk {} not found", &config.disk_path));
        }

        let dev_info = DevInfo::new(&config.disk_path)?;

        let _disk = File::open(dev_info.rdisk())?.acquire_lock(if config.read_only {
            FlockKind::Shared
        } else {
            FlockKind::Exclusive
        })?;

        host_println!("disk: {}", dev_info.disk());
        host_println!("rdisk: {}", dev_info.rdisk());
        host_println!("label: {:?}", dev_info.label());
        host_println!("fs_type: {:?}", dev_info.fs_type());
        host_println!("uuid: {:?}", dev_info.uuid());
        host_println!("mount name: {}", dev_info.auto_mount_name());

        let mut can_detach = true;
        let session_pgid = unsafe { libc::setsid() };
        if session_pgid < 0 {
            host_eprintln!("Failed to setsid, cannot run in the background");
            can_detach = false;
        }

        let mut gvproxy = start_gvproxy(&config.common)?;
        let gvproxy_pid = gvproxy.id() as libc::pid_t;
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

            let ctx = setup_vm(&config, &dev_info, true).context("Failed to setup microVM")?;
            start_vmproxy(ctx, &config, &dev_info, || forked.redirect())
                .context("Failed to start microVM")?;
        } else {
            // Parent process
            let child_pid = forked.pid;
            let vm_wait_action = deferred.add(move || {
                _ = wait_for_vm_status(child_pid);
            });

            let rt_info = Arc::new(Mutex::new(api::RuntimeInfo {
                mount_config: config.clone(),
                dev_info: dev_info.clone(),
                session_pgid,
                vmm_pid: child_pid,
                gvproxy_pid,
                mount_point: None,
            }));
            api::serve_info(rt_info.clone());

            let (nfs_ready_tx, nfs_ready_rx) = mpsc::channel();

            // Spawn a thread to read from the pipe
            _ = thread::spawn(move || {
                let mut nfs_ready = false;
                let mut exit_code = None;
                let mut buf_reader = BufReader::new(unsafe { File::from_raw_fd(forked.out_fd) });
                let mut line = String::new();
                while let Ok(bytes) = buf_reader.read_line(&mut line) {
                    let mut skip_line = false;
                    if bytes == 0 {
                        break; // EOF
                    }
                    if line.contains("READY AND WAITING FOR NFS CLIENT CONNECTIONS") {
                        // Notify the main thread that NFS server is ready
                        nfs_ready_tx.send(NfsStatus::Ready).unwrap();
                        nfs_ready = true;
                    } else if line.starts_with("<anylinuxfs-exit-code") {
                        skip_line = true;
                        exit_code = line
                            .split(':')
                            .nth(1)
                            .map(|pattern| {
                                pattern
                                    .trim()
                                    .strip_suffix(">")
                                    .unwrap_or(pattern)
                                    .parse::<i32>()
                                    .ok()
                            })
                            .flatten();
                    }

                    if !skip_line {
                        guest_println!("{}", line.trim_end());
                    }
                    line.clear();
                }
                if !nfs_ready {
                    nfs_ready_tx.send(NfsStatus::Failed(exit_code)).unwrap();
                }
            });

            // drop privileges back to the original user if he used sudo
            drop_privileges(config.common.sudo_uid, config.common.sudo_gid)?;

            let nfs_status =
                wait_for_nfs_server(111, nfs_ready_rx).unwrap_or(NfsStatus::Failed(None));
            if nfs_status.ok() {
                host_println!("Port 111 open, NFS server ready");

                let event_session = diskutil::EventSession::new()?;
                // mount nfs share
                let share_name = dev_info.auto_mount_name();
                match mount_nfs(&share_name) {
                    Ok(_) => host_println!("Requested NFS share mount"),
                    Err(e) => host_eprintln!("Failed to request NFS mount: {:#}", e),
                }
                let nfs_path = PathBuf::from(format!("localhost:/mnt/{}", share_name));
                let mount_point_opt = event_session.wait_for_mount(&nfs_path);

                if let Some(mount_point) = &mount_point_opt {
                    host_println!(
                        "{} was mounted as {}",
                        dev_info.disk(),
                        mount_point.display()
                    );

                    rt_info.lock().unwrap().mount_point = Some(mount_point.display().into());
                }

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
                send_quit_cmd(&config.common)?;
            } else {
                host_println!("NFS server not ready");
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

    fn run_mount_parent(&mut self, forked: utils::ForkOutput) -> anyhow::Result<()> {
        let comm_read_fd = forked.out_fd;
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
        let _lock_file = LockFile::new(LOCK_FILE)?.acquire_lock(FlockKind::Exclusive)?;
        let config = load_config()?;
        init_rootfs(&config, true)?;

        Ok(())
    }

    fn run_config(&mut self, cmd: ConfigCmd) -> anyhow::Result<()> {
        let config = load_config()?;
        let config_file_path = &config.config_file_path;

        let mut krun_config = config.krun;

        if cmd == ConfigCmd::default() {
            println!("{}", &krun_config);
            return Ok(());
        }

        if let Some(log_level) = cmd.log_level {
            krun_config.set_log_level(log_level);
        }
        if let Some(num_vcpus) = cmd.num_vcpus {
            krun_config.num_vcpus = num_vcpus;
        }
        if let Some(ram_size_mib) = cmd.ram_size_mib {
            krun_config.ram_size_mib = ram_size_mib;
        }

        save_krun_config(&krun_config, config_file_path)?;
        println!("{}", &krun_config);

        Ok(())
    }

    fn run_list(&mut self) -> anyhow::Result<()> {
        let config = load_config()?;
        println!("{}", diskutil::list_linux_partitions(config)?);
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
            safe_println!("{}", line.trim_end())?;
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
            Ok(api::Response::Config(rt_info)) => {
                let mount_point = match validated_mount_point(&rt_info) {
                    MountStatus::Mounted(mount_point) => mount_point,
                    MountStatus::NoLonger => {
                        eprintln!(
                            "Drive {} no longer mounted but anylinuxfs is still running; try `anylinuxfs stop`.",
                            &rt_info.mount_config.disk_path
                        );
                        return Err(StatusError::new("Mount point is not valid", 1).into());
                    }
                    MountStatus::NotYet => {
                        eprintln!(
                            "Drive {} not mounted yet, please wait",
                            &rt_info.mount_config.disk_path
                        );
                        return Ok(());
                    }
                };

                let info: Vec<_> = rt_info
                    .dev_info
                    .fs_type()
                    .into_iter()
                    .chain(
                        rt_info
                            .mount_config
                            .mount_options
                            .iter()
                            .flat_map(|opts| opts.split(',')),
                    )
                    .collect();

                let user_name =
                    User::from_uid(Uid::from_raw(rt_info.mount_config.common.invoker_uid))
                        .ok()
                        .flatten()
                        .map(|u| u.name)
                        .unwrap_or("<unknown>".into());

                println!(
                    "{} on {} ({}, mounted by {}) VM[cpus: {}, ram: {} MiB]",
                    &rt_info.mount_config.disk_path,
                    mount_point.display(),
                    info.join(", "),
                    &user_name,
                    rt_info.mount_config.common.krun.num_vcpus,
                    rt_info.mount_config.common.krun.ram_size_mib,
                );
            }
            Err(err) => {
                if let Some(err) = err.downcast_ref::<io::Error>() {
                    match err.kind() {
                        io::ErrorKind::ConnectionRefused => return Ok(()),
                        io::ErrorKind::NotFound => return Ok(()),
                        _ => (),
                    }
                }
                return Err(err);
            }
        }

        Ok(())
    }

    fn run_stop(&mut self, cmd: StopCmd) -> anyhow::Result<()> {
        let resp = api::Client::make_request(api::Request::GetConfig);

        match resp {
            Ok(api::Response::Config(rt_info)) => {
                if !cmd.force {
                    // try to trigger normal shutdown first
                    if let MountStatus::Mounted(mount_point) = validated_mount_point(&rt_info) {
                        println!("Unmounting {}...", mount_point.display());
                        unmount_nfs(&mount_point)?;
                        return Ok(());
                    };
                    println!("Already unmounted, shutting down...");
                    // not killing the whole process group, just the session leader;
                    // this should trigger graceful shutdown of the VMM and its parent
                    if unsafe { libc::kill(rt_info.session_pgid, libc::SIGTERM) } < 0 {
                        return Err(io::Error::last_os_error())
                            .context(format!("Failed to send SIGTERM to anylinuxfs"));
                    }
                } else {
                    if let MountStatus::Mounted(mount_point) = validated_mount_point(&rt_info) {
                        print!(
                            "This action will force kill anylinuxfs. You should first unmount {} if possible.\nDo you want to proceed anyway? [y/N] ",
                            mount_point.display()
                        );
                        io::stdout().flush()?;
                        let mut input = String::new();
                        io::stdin().read_line(&mut input)?;
                        if !matches!(input.trim().to_lowercase().as_str(), "y" | "yes") {
                            return Ok(());
                        }
                    }
                    println!("Trying to shutdown anylinuxfs VM directly...");
                    let mut vm_exited_gracefully = false;
                    if send_quit_cmd(&rt_info.mount_config.common).is_ok() {
                        // wait for vmm process to exit or become zombie
                        vm_exited_gracefully = wait_for_proc_exit(rt_info.vmm_pid).is_ok();
                    }
                    if vm_exited_gracefully {
                        println!("VM exited gracefully, killing the remaining processes...");
                    } else {
                        println!("Killing anylinuxfs processes...");
                    }
                    if unsafe { libc::kill(rt_info.gvproxy_pid, libc::SIGTERM) } == 0 {
                        // gvproxy could still terminate gracefully
                        if wait_for_proc_exit(rt_info.gvproxy_pid).is_ok() {
                            println!("gvproxy exited gracefully");
                        }
                    }
                    if unsafe { libc::killpg(rt_info.session_pgid, libc::SIGKILL) } < 0 {
                        let last_error = io::Error::last_os_error();
                        if last_error.raw_os_error().unwrap() != libc::ESRCH {
                            return Err(last_error)
                                .context(format!("Failed to send SIGKILL to anylinuxfs"));
                        }
                    }
                    _ = vsock_cleanup(&rt_info.mount_config.common);
                    _ = gvproxy_cleanup(&rt_info.mount_config.common);
                }
            }
            Err(err) => {
                if let Some(err) = err.downcast_ref::<io::Error>() {
                    match err.kind() {
                        io::ErrorKind::ConnectionRefused => return Ok(()),
                        io::ErrorKind::NotFound => return Ok(()),
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
            Commands::Config(cmd) => self.run_config(cmd),
            Commands::List => self.run_list(),
            Commands::Stop(cmd) => self.run_stop(cmd),
            Commands::Shell(cmd) => self.run_shell(cmd),
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
