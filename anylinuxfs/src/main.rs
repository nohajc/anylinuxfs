use anyhow::{Context, anyhow};
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use common_utils::{
    CustomActionConfig, Deferred, host_eprintln, host_println, log, prefix_eprintln,
    prefix_println, safe_println,
};

use devinfo::DevInfo;
use nanoid::nanoid;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::env::VarError;
use std::fmt::Display;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddrV4, TcpStream};
use std::os::fd::FromRawFd;
use std::os::unix::fs::chown;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::ptr::null;
use std::time::Duration;
use std::{
    env,
    ffi::CString,
    fs::remove_file,
    io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};
use std::{iter, thread};

use notify::{RecursiveMode, Watcher};
use std::sync::{Arc, Mutex, mpsc};
use utils::{
    AcquireLock, CommFd, FlockKind, HasCommFd, HasPipeInFd, HasPipeOutFds, HasPtyFd, LockFile,
    OutputAction, PassthroughBufReader, StatusError, write_to_pipe,
};

use crate::fsutil::mount_nfs_subdirs;
use crate::utils::{ToCStringVec, ToPtrVec};

mod api;
#[allow(unused)]
mod bindings;
mod devinfo;
mod diskutil;
mod dnsutil;
mod fsutil;
mod pubsub;
mod rpcbind;
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
    exec_path: PathBuf,
    root_path: PathBuf,
    root_ver_file_path: PathBuf,
    config_file_path: PathBuf,
    log_file_path: PathBuf,
    init_rootfs_path: PathBuf,
    kernel_path: PathBuf,
    gvproxy_path: PathBuf,
    gvproxy_log_path: PathBuf,
    vsock_path: String,
    vfkit_sock_path: String,
    invoker_uid: libc::uid_t,
    invoker_gid: libc::gid_t,
    sudo_uid: Option<libc::uid_t>,
    sudo_gid: Option<libc::gid_t>,
    passphrase_config: PassphrasePromptConfig,
    preferences: Preferences,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Preferences {
    #[serde(default)]
    alpine: AlpineConfig,
    #[serde(default)]
    custom_actions: HashMap<String, CustomActionConfig>,
    #[serde(default)]
    gvproxy: GvproxyConfig,
    #[serde(default)]
    krun: KrunConfig,
    #[serde(default)]
    misc: MiscConfig,
    // legacy config
    #[serde(rename = "log_level")]
    log_level_numeric: Option<u32>,
    num_vcpus: Option<u8>,
    ram_size_mib: Option<u32>,
}

impl Display for Preferences {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[krun]\n{}", self.krun)?;
        write!(f, "\n\n[misc]\n{}", self.misc)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct AlpineConfig {
    custom_packages: Vec<String>,
}

trait CustomActionEnvironment {
    fn prepare_environment(&self) -> anyhow::Result<Vec<String>>;
}

impl CustomActionEnvironment for CustomActionConfig {
    fn prepare_environment(&self) -> anyhow::Result<Vec<String>> {
        let mut referenced_variables = HashSet::new();
        for script in self.all_scripts() {
            referenced_variables.extend(utils::find_env_vars(script));
        }

        let mut predefined_vars = HashSet::new();
        let mut undefined_vars = Vec::new();
        let mut env_vars = Vec::new();
        for var_str in &self.environment {
            let var_name = var_str
                .split('=')
                .next()
                .ok_or_else(|| anyhow!("invalid environment variable format: {}", var_str))?;
            predefined_vars.insert(var_name.to_owned());
            env_vars.push(var_str.clone());
        }
        for var_name in referenced_variables.iter().chain(&self.capture_environment) {
            match env::var(&var_name) {
                Ok(var_value) => {
                    env_vars.push(format!("{}={}", var_name, var_value));
                }
                Err(VarError::NotPresent) => {
                    if !Self::VM_EXPORTED_VARS.contains(&var_name.as_str())
                        && !predefined_vars.contains(var_name)
                    {
                        undefined_vars.push(var_name.clone());
                    }
                }
                Err(VarError::NotUnicode(value)) => {
                    return Err(anyhow::anyhow!(
                        "environment variable '{}' contains non-UTF-8 characters: {}",
                        var_name,
                        value.display()
                    ));
                }
            }
        }

        if !undefined_vars.is_empty() {
            let var_list = undefined_vars.join(", ");
            return Err(anyhow::anyhow!(
                "required environment variables not defined: {}",
                var_list
            ));
        }
        Ok(env_vars)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct GvproxyConfig {
    debug: bool,
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

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct MiscConfig {
    passphrase_config: PassphrasePromptConfig,
}

impl Display for MiscConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "passphrase_config = {}", self.passphrase_config)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct MountConfig {
    disk_path: String,
    read_only: bool,
    mount_options: Option<String>,
    allow_remount: bool,
    custom_mount_point: Option<PathBuf>,
    fs_driver: Option<String>,
    bind_addr: IpAddr,
    verbose: bool,
    open_finder: bool,
    common: Config,
    custom_action: Option<String>,
}

impl MountConfig {
    fn get_action(&self) -> Option<&CustomActionConfig> {
        match &self.custom_action {
            Some(action_name) => self.common.preferences.custom_actions.get(action_name),
            None => None,
        }
    }
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

#[derive(Debug, Clone, Copy, ValueEnum, Deserialize, Serialize, PartialEq, Eq)]
enum PassphrasePromptConfig {
    #[clap(name = "a")]
    #[serde(rename = "ask_for_each")]
    AskForEach,
    #[clap(name = "1")]
    #[serde(rename = "one_for_all")]
    OneForAll,
}

impl Default for PassphrasePromptConfig {
    fn default() -> Self {
        PassphrasePromptConfig::AskForEach
    }
}

impl Display for PassphrasePromptConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let val = match self {
            PassphrasePromptConfig::AskForEach => "ask_for_each",
            PassphrasePromptConfig::OneForAll => "one_for_all",
        };
        write!(f, "{}", val)
    }
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
    #[command(after_help = "Things you can mount:
- physical partitions
- raw disk images
- LVM or RAID volumes spanning one or more disks
- LUKS-encrypted partitions
- BitLocker-encrypted partitions
- LVM/RAID on LUKS
- multi-disk btrfs filesystems

Supported partition schemes:
- GPT
- MBR
- disk without partitions (single filesystem or LVM/LUKS container).")]
    Mount(MountCmd),
    /// Unmount a filesystem
    Unmount(UnmountCmd),
    /// Init Linux rootfs (can be used to reinitialize virtual environment)
    Init,
    /// Show status information (mount parameters, vm resources, etc.)
    Status,
    /// Show log of current or previous run
    Log(LogCmd),
    /// Configure microVM parameters and other miscellaneous settings
    Config(ConfigCmd),
    /// List all available disks with compatible filesystems (run with sudo to get more detailed info)
    #[command(
        after_help = "Lists all physical partitions and LVM/RAID volumes. Can decrypt LUKS partition metadata too."
    )]
    List(ListCmd),
    /// Stop anylinuxfs (can be used if unresponsive)
    Stop(StopCmd),
    /// microVM shell for debugging (configures the VM according to mount options but only starts a shell)
    Shell(ShellCmd),
    /// Show dmesg output from the last run
    Dmesg,
    /// Manage custom alpine packages
    #[command(subcommand)]
    Apk(ApkCmd),
    #[command(subcommand, hide = true)]
    Rpcbind(RpcBindCmd),
}

#[derive(Args, Default, PartialEq, Eq)]
struct CommonArgs {
    /// Passphrase configuration (ask for each drive / use one for all)
    #[arg(short, long)]
    passphrase_config: Option<PassphrasePromptConfig>,
}

#[derive(Args)]
struct MountCmd {
    /// File path(s), LVM identifier or RAID identifier, e.g.:
    /// /dev/diskXsY[:/dev/diskYsZ:...]
    /// lvm:<vg-name>:diskXsY[:diskYsZ:...]:<lv-name>
    /// raid:diskXsY[:diskYsZ:...]
    /// (see `list` command output for available volumes)
    #[clap(verbatim_doc_comment)]
    disk_ident: String,
    /// Custom mount path to override the default under /Volumes
    mount_point: Option<String>,
    /// Options passed to the Linux mount command
    #[arg(short, long)]
    options: Option<String>,
    /// Allow remount: proceed even if the disk is already mounted by macOS (NTFS, exFAT)
    #[arg(short, long)]
    remount: bool,
    /// Name of a custom action to perform after mounting (defined in config.toml)
    #[arg(short, long)]
    action: Option<String>,
    /// Filesystem driver override (e.g. for using ntfs3 instead of ntfs-3g)
    #[arg(short = 't', long = "type")]
    fs_driver: Option<String>,
    #[command(flatten)]
    common: CommonArgs,
    /// Open Finder window with the mounted drive
    #[arg(short, long, default_value = "true")]
    window: std::primitive::bool,
    /// Override this to share the mount to a different machine
    #[arg(short, long, default_value = "127.0.0.1")]
    bind_addr: String,
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Args)]
struct UnmountCmd {
    /// Disk identifier or mount point (unmounts all if not specified)
    #[arg(id = "DISK_IDENT|MOUNT_POINT")]
    path: Option<String>,
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
    #[command(flatten)]
    common: CommonArgs,
}

#[derive(Args)]
struct ListCmd {
    /// Decrypt LUKS partitions: comma-separated list of paths or "all"
    #[arg(short, long, value_delimiter = ',', num_args = 1..)]
    decrypt: Option<Vec<String>>,
    /// Show Microsoft filesystems (NTFS, exFAT) instead of Linux filesystems
    #[arg(short, long)]
    microsoft: bool,
    #[command(flatten)]
    common: CommonArgs,
}

#[derive(Args)]
struct StopCmd {
    /// Force stop the VM
    #[arg(short, long)]
    force: bool,
}

#[derive(Args)]
struct ShellCmd {
    /// Command to run in the shell
    #[arg(short, long)]
    command: Option<String>,
    /// Skip initialization of virtual environment
    #[arg(short, long)]
    skip_init: bool,
    #[command(flatten)]
    mnt: MountCmd,
}

#[derive(Subcommand)]
enum ApkCmd {
    /// List custom packages
    Info,
    /// Install custom packages
    Add {
        /// Packages to install
        packages: Vec<String>,
    },
    /// Remove custom packages
    Del {
        /// Packages to remove
        packages: Vec<String>,
    },
}

#[derive(Subcommand)]
enum RpcBindCmd {
    /// Register RPC services
    Register,
    /// Unregister RPC services
    Unregister,
    /// List registered RPC services
    List,
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
        let mount_cmd_usage =
            "\x1b[1manylinuxfs [mount]\x1b[0m [OPTIONS] <DISK_IDENT> [MOUNT_POINT]";
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

fn load_config(common_args: &CommonArgs) -> anyhow::Result<Config> {
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

    let exec_path = fs::canonicalize(env::current_exe().context("Failed to get executable path")?)
        .context("Failed to get resolved exec path")?;
    let exec_dir = exec_path
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
    let log_dir = home_dir.join("Library").join("Logs");
    let log_file_path = log_dir.join("anylinuxfs.log");
    let gvproxy_log_path = log_dir.join("gvproxy.log");

    let libexec_dir = prefix_dir.join("libexec");
    let init_rootfs_path = libexec_dir.join("init-rootfs").to_owned();
    let kernel_path = libexec_dir.join("Image").to_owned();
    let gvproxy_path = libexec_dir.join("gvproxy").to_owned();

    let vsock_path = format!("/tmp/anylinuxfs-{}-vsock", rand_string(8));
    let vfkit_sock_path = format!("/tmp/vfkit-{}.sock", rand_string(8));

    let preferences = load_preferences(&config_file_path)?;
    // println!("Loaded preferences: {:#?}", &preferences);

    let passphrase_config = common_args
        .passphrase_config
        .unwrap_or(preferences.misc.passphrase_config);

    Ok(Config {
        exec_path,
        root_path,
        root_ver_file_path,
        config_file_path,
        log_file_path,
        init_rootfs_path,
        kernel_path,
        gvproxy_path,
        gvproxy_log_path,
        vsock_path,
        vfkit_sock_path,
        invoker_uid,
        invoker_gid,
        sudo_uid,
        sudo_gid,
        passphrase_config,
        preferences,
    })
}

fn convert_legacy_config(config: &mut Preferences) {
    if let Some(log_level_numeric) = config.log_level_numeric.take() {
        config.krun.log_level_numeric = log_level_numeric;
    }
    if let Some(num_vcpus) = config.num_vcpus.take() {
        config.krun.num_vcpus = num_vcpus;
    }
    if let Some(ram_size_mib) = config.ram_size_mib.take() {
        config.krun.ram_size_mib = ram_size_mib;
    }
}

fn load_preferences(path: &Path) -> anyhow::Result<Preferences> {
    match fs::read_to_string(path) {
        Ok(config_str) => {
            let mut config: Preferences = toml::from_str(&config_str)
                .context(format!("Failed to parse config file {}", path.display()))?;
            convert_legacy_config(&mut config);
            Ok(config)
        }
        Err(_) => Ok(Preferences::default()),
    }
}

fn save_preferences(preferences: &Preferences, config_file_path: &Path) -> anyhow::Result<()> {
    let config_str =
        toml::to_string(preferences).context("Failed to serialize Preferences to TOML")?;
    fs::write(config_file_path, config_str).context(format!(
        "Failed to write config file {}",
        config_file_path.display()
    ))?;
    Ok(())
}

fn load_mount_config(cmd: MountCmd) -> anyhow::Result<MountConfig> {
    let common = load_config(&cmd.common)?;

    let (disk_path, mount_options) = if !cmd.disk_ident.is_empty() {
        (cmd.disk_ident, cmd.options)
    } else {
        host_eprintln!("No disk path provided");
        std::process::exit(1);
    };

    let allow_remount = cmd.remount;
    let custom_mount_point = match cmd.mount_point {
        Some(path) => {
            let path = fs::canonicalize(&path)
                .with_context(|| format!("Failed to resolve path {}", &path))?;

            if !fs::metadata(&path)
                .with_context(|| format!("Failed to get metadata for {}", &path.display()))?
                .is_dir()
            {
                return Err(anyhow!("{} is not a directory", &path.display()));
            }

            Some(path)
        }
        None => None,
    };

    let bind_addr = cmd
        .bind_addr
        .parse()
        .with_context(|| format!("invalid IP address given: {}", &cmd.bind_addr))?;
    let read_only = is_read_only_set(mount_options.as_deref());
    let verbose = cmd.verbose;

    let fs_driver = cmd.fs_driver;

    if bind_addr != Ipv4Addr::UNSPECIFIED && bind_addr != Ipv4Addr::LOCALHOST {
        // check if the given bind address is assigned to any interface
        if let Ok(interfaces) = if_addrs::get_if_addrs() {
            if !interfaces.iter().any(|iface| iface.ip() == bind_addr) {
                return Err(anyhow::anyhow!(
                    "Bind address {} is not assigned to any interface",
                    bind_addr
                ));
            }
        }
    }

    let open_finder = cmd.window;

    let custom_action = if let Some(action_name) = cmd.action {
        match common.preferences.custom_actions.get(&action_name) {
            Some(_) => Some(action_name),
            None => {
                return Err(anyhow::anyhow!("unknown custom action: {}", action_name));
            }
        }
    } else {
        None
    };

    Ok(MountConfig {
        disk_path,
        read_only,
        mount_options,
        allow_remount,
        custom_mount_point,
        fs_driver,
        bind_addr,
        verbose,
        open_finder,
        common,
        custom_action,
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

fn setup_vm(
    config: &Config,
    dev_info: &[DevInfo],
    use_gvproxy: bool,
    use_vsock: bool,
    add_disks_ro: bool,
) -> anyhow::Result<u32> {
    let ctx = unsafe { bindings::krun_create_ctx() }.context("Failed to create context")?;

    let level = config.preferences.krun.log_level_numeric;
    unsafe { bindings::krun_set_log_level(level) }.context("Failed to set log level")?;

    let num_vcpus = config.preferences.krun.num_vcpus;
    let ram_mib = config.preferences.krun.ram_size_mib;
    unsafe { bindings::krun_set_vm_config(ctx, num_vcpus, ram_mib) }
        .context("Failed to set VM config")?;

    // run vmm as the original user if he used sudo
    if let Some(uid) = config.sudo_uid {
        unsafe { bindings::krun_setuid(ctx, uid) }.context("Failed to set vmm uid")?;
    }

    if let Some(gid) = config.sudo_gid {
        unsafe { bindings::krun_setgid(ctx, gid) }.context("Failed to set vmm gid")?;
    }

    unsafe { bindings::krun_set_root(ctx, CString::from_path(&config.root_path).as_ptr()) }
        .context("Failed to set root")?;

    for (i, di) in dev_info.iter().enumerate() {
        unsafe {
            bindings::krun_add_disk(
                ctx,
                CString::new(format!("data{}", i)).unwrap().as_ptr(),
                CString::new(di.rdisk()).unwrap().as_ptr(),
                add_disks_ro,
            )
        }
        .context("Failed to add disk")?;
    }

    if use_gvproxy {
        unsafe {
            bindings::krun_set_gvproxy_path(
                ctx,
                CString::new(config.vfkit_sock_path.as_str())
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

    vsock_cleanup(&config)?;

    if use_vsock {
        unsafe {
            bindings::krun_add_vsock_port2(
                ctx,
                12700,
                CString::new(config.vsock_path.as_str()).unwrap().as_ptr(),
                true,
            )
        }
        .context("Failed to add vsock port")?;
    }

    unsafe { bindings::krun_set_workdir(ctx, c"/".as_ptr()) }.context("Failed to set workdir")?;

    let cmdline = c"reboot=k panic=-1 panic_print=0 console=hvc0 rootfstype=virtiofs rw quiet no-kvmapf init=/init.krun";

    unsafe {
        bindings::krun_set_kernel(
            ctx,
            CString::from_path(&config.kernel_path).as_ptr(),
            0, // KRUN_KERNEL_FORMAT_RAW
            null(),
            cmdline.as_ptr(),
        )
    }
    .context("Failed to set kernel")?;

    Ok(ctx)
}

fn start_vmproxy(
    ctx: u32,
    config: &MountConfig,
    service_status: &ServiceStatus,
    env: &[String],
    dev_info: &DevInfo,
    multi_device: bool,
    to_decrypt: Vec<String>,
    before_start: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let to_decrypt_arg = if to_decrypt.is_empty() {
        None
    } else {
        Some(to_decrypt.join(","))
    };

    let reuse_passphrase = config.common.passphrase_config == PassphrasePromptConfig::OneForAll;

    let args: Vec<_> = [
        c"/vmproxy".to_owned(),
        CString::new(dev_info.vm_path()).unwrap(),
        CString::new(dev_info.auto_mount_name()).unwrap(),
        c"-b".to_owned(),
        CString::new(config.bind_addr.to_string()).unwrap(),
    ]
    .into_iter()
    .chain([
        c"-t".to_owned(),
        CString::new(dev_info.fs_type().unwrap_or("auto")).unwrap(),
    ])
    .chain(
        config
            .fs_driver
            .as_deref()
            .into_iter()
            .flat_map(|fs_driver| [c"--fs-driver".to_owned(), CString::new(fs_driver).unwrap()]),
    )
    .chain(
        config
            .mount_options
            .as_deref()
            .into_iter()
            .flat_map(|opts| [c"-o".to_owned(), CString::new(opts).unwrap()]),
    )
    .chain(
        to_decrypt_arg
            .as_deref()
            .into_iter()
            .flat_map(|d| vec![c"-d".to_owned(), CString::new(d).unwrap()]),
    )
    .chain(config.get_action().into_iter().flat_map(|action| {
        vec![
            c"-a".to_owned(),
            CString::new(action.percent_encode().expect("failed to serialize action")).unwrap(),
        ]
    }))
    .chain(multi_device.then_some(c"-m".to_owned()).into_iter())
    .chain(reuse_passphrase.then_some(c"-r".to_owned()).into_iter())
    .chain(
        service_status
            .rpcbind_running
            .then_some(c"-h".to_owned())
            .into_iter(),
    )
    .chain(config.verbose.then_some(c"-v".to_owned()).into_iter())
    .collect();

    host_println!("vmproxy args: {:?}", &args);

    let argv = args.to_ptr_vec();

    let cenv = env.to_cstring_vec();
    let envp = cenv.to_ptr_vec();

    unsafe { bindings::krun_set_exec(ctx, argv[0], argv[1..].as_ptr(), envp.as_ptr()) }
        .context("Failed to set exec")?;

    before_start().context("Before start callback failed")?;
    unsafe { bindings::krun_start_enter(ctx) }.context("Failed to start VM")?;

    Ok(())
}

fn start_vmshell_forked(ctx: u32, command: Option<String>, env: &[String]) -> anyhow::Result<i32> {
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error()).context("Failed to fork process");
    } else if pid == 0 {
        // Child process
        start_vmshell(ctx, command, env)?;
        unreachable!();
    } else {
        // Parent process
        let mut status = 0;
        if unsafe { libc::waitpid(pid, &mut status, 0) } < 0 {
            return Err(io::Error::last_os_error()).context("Failed to wait for child process");
        }
        return Ok(to_exit_code(status));
    }
}

fn start_vmshell(ctx: u32, command: Option<String>, env: &[String]) -> anyhow::Result<()> {
    let mut args = vec![c"/bin/bash".to_owned()];
    if let Some(cmd) = command {
        args.extend_from_slice(&[c"-c".to_owned(), CString::new(cmd).unwrap()]);
    }

    let argv = args.to_ptr_vec();

    let cenv = env.to_cstring_vec();
    let envp = cenv.to_ptr_vec();

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
    config: &Config,
    dev_info: &[DevInfo],
    use_gvproxy: bool,
    add_disks_ro: bool,
    args: Vec<CString>,
    process_stdin: Option<impl FnOnce(libc::c_int) -> anyhow::Result<()>>,
) -> anyhow::Result<VMOutput> {
    let forked = utils::fork_with_piped_output()?;
    if forked.pid == 0 {
        // child process
        let ctx = setup_vm(config, dev_info, use_gvproxy, false, add_disks_ro)?;

        let argv = args.to_ptr_vec();
        let envp = vec![std::ptr::null()];

        unsafe { bindings::krun_set_exec(ctx, argv[0], argv[1..].as_ptr(), envp.as_ptr()) }
            .context("Failed to set exec")?;

        unsafe { bindings::krun_start_enter(ctx) }.context("Failed to start VM")?;
        unreachable!();
    } else {
        // parent process
        if let Some(process_stdin) = process_stdin {
            process_stdin(forked.in_fd())?;
        }

        let stdout = read_all_from_fd(forked.out_fd())?;
        let stderr = read_all_from_fd(forked.err_fd())?;

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
    let mut gvproxy_args = vec![
        "--listen",
        &net_sock_uri,
        "--listen-vfkit",
        &vfkit_sock_uri,
        "--ssh-port",
        "-1",
    ];

    if config.preferences.gvproxy.debug {
        gvproxy_args.push("--debug");
    }

    let mut gvproxy_cmd = Command::new(&config.gvproxy_path);

    let gvproxy_out =
        File::create(&config.gvproxy_log_path).context("Failed to create gvproxy.log file")?;
    let gvproxy_err =
        File::try_clone(&gvproxy_out).context("Failed to clone gvproxy.log file handle")?;

    gvproxy_cmd
        .args(&gvproxy_args)
        .stdout(gvproxy_out)
        .stderr(gvproxy_err);

    if let (Some(uid), Some(gid)) = (config.sudo_uid, config.sudo_gid) {
        // run gvproxy with dropped privileges
        gvproxy_cmd.uid(uid).gid(gid);
    }

    let gvproxy_process = gvproxy_cmd
        .spawn()
        .context("Failed to start gvproxy process")?;

    Ok(gvproxy_process)
}

#[derive(Debug)]
enum NfsStatus {
    Ready(NfsReadyState),
    Failed(Option<i32>),
}

#[derive(Debug)]
struct NfsReadyState {
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

fn mount_nfs(share_path: &str, config: &MountConfig, vers4: bool) -> anyhow::Result<()> {
    let status = if let Some(mount_point) = config.custom_mount_point.as_deref() {
        let opts = if vers4 { "-o vers=4" } else { "" };
        let mut shell_script = format!(
            "mount -t nfs {} \"localhost:{}\" \"{}\"",
            opts,
            share_path,
            mount_point.display()
        );
        if config.open_finder {
            shell_script += &format!(" && open \"{}\"", mount_point.display());
        }

        // try to run mount as regular user first
        // (if that succeeds, umount will work without sudo)
        let mut status = Command::new("sh")
            .arg("-c")
            .arg(&shell_script)
            .uid(config.common.invoker_uid)
            .gid(config.common.invoker_gid)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;

        if !status.success() {
            // otherwise run as root (probably the mount point wasn't accessible)
            status = Command::new("sh").arg("-c").arg(&shell_script).status()?;
        }
        status
    } else {
        let location = format!("nfs://localhost:{}", share_path);
        let apple_script = if config.open_finder {
            format!(
                "tell application \"Finder\" to open location \"{}\"",
                location
            )
        } else {
            format!("mount volume \"{}\"", location)
        };
        Command::new("osascript")
            .arg("-e")
            .arg(apple_script)
            .stdout(Stdio::null())
            .status()?
    };

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

fn unmount_fs(volume_path: &Path) -> anyhow::Result<()> {
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

fn send_quit_cmd(config: &Config) -> anyhow::Result<()> {
    let mut stream = UnixStream::connect(&config.vsock_path)?;

    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    stream.write_all(b"quit\n")?;
    stream.flush()?;

    // we don't care about the response contents
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
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

const ROOTFS_CURRENT_VERSION: &str = "1.2.0";

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

    let dns_server = dnsutil::get_dns_server_with_fallback();

    let mut hnd = init_rootfs_cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .args(&["-n", &dns_server])
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
    host_println!("libkrun VM exited with status: {}", to_exit_code(status));
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

fn validated_mount_point(rt_info: &api::RuntimeInfo) -> MountStatus<'_> {
    let Some(mount_point) = rt_info.mount_point.as_ref().map(Path::new) else {
        return MountStatus::NotYet;
    };

    let expected_mount_point = match rt_info.mount_config.get_action() {
        Some(action) if !action.override_nfs_export.is_empty() => {
            action.override_nfs_export.clone()
        }
        _ => format!("/mnt/{}", rt_info.dev_info.auto_mount_name()),
    };
    let expected_mount_dev = PathBuf::from(format!("localhost:{}", expected_mount_point));
    match fsutil::mounted_from(&mount_point) {
        Ok(mount_dev) if mount_dev == expected_mount_dev => MountStatus::Mounted(mount_point),
        _ => MountStatus::NoLonger,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DevType {
    Direct,
    PV,
    LV,
}

fn print_dev_info(dev_info: &DevInfo, dev_type: DevType) {
    if dev_type == DevType::Direct || dev_type == DevType::PV {
        host_println!("disk: {}", dev_info.disk());
        host_println!("rdisk: {}", dev_info.rdisk());
    }

    if dev_type == DevType::Direct || dev_type == DevType::LV {
        host_println!("label: {:?}", dev_info.label());
        host_println!("fs_type: {:?}", dev_info.fs_type());
        host_println!("uuid: {:?}", dev_info.uuid());
        host_println!("mount name: {}", dev_info.auto_mount_name());
    }
}

fn claim_devices(config: &MountConfig) -> anyhow::Result<(Vec<DevInfo>, DevInfo, Vec<File>)> {
    let mount_table = fsutil::MountTable::new()?;
    // host_println!("Current mount table: {:#?}", mount_table);

    let mut dev_infos = Vec::new();
    let mut disks = Vec::new();

    let disk_path = config.disk_path.as_str();

    let mut mnt_dev_info = if disk_path.starts_with("lvm:") {
        // example: lvm:vg1:disk7s1:lvol0
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
            let pv_path = format!("/dev/{}", di);
            if mount_table.is_mounted(&pv_path) {
                return Err(anyhow!("{} is already mounted", &pv_path));
            }
            let dev_info = DevInfo::pv(&pv_path)?;
            let disk = File::open(dev_info.rdisk())?.acquire_lock(if config.read_only {
                FlockKind::Shared
            } else {
                FlockKind::Exclusive
            })?;

            print_dev_info(&dev_info, DevType::PV);

            dev_infos.push(dev_info);
            disks.push(disk);
        }

        // fs label will be obtained later from the VM output
        let lv_info = DevInfo::lv(disk_path, None, vm_path)?;
        print_dev_info(&lv_info, DevType::LV);
        lv_info
    } else if disk_path.starts_with("raid:") {
        // example: raid:disk7s1:disk8s1
        let disk_ident: Vec<&str> = disk_path.split(':').collect();
        if disk_ident.len() < 2 {
            return Err(anyhow!("Invalid RAID disk path"));
        }

        let vm_path = "/dev/md127";

        for (_, &di) in disk_ident.iter().skip(1).enumerate() {
            let pv_path = format!("/dev/{}", di);
            if mount_table.is_mounted(&pv_path) {
                return Err(anyhow!("{} is already mounted", &pv_path));
            }
            let dev_info = DevInfo::pv(&pv_path)?;
            let disk = File::open(dev_info.rdisk())?.acquire_lock(if config.read_only {
                FlockKind::Shared
            } else {
                FlockKind::Exclusive
            })?;

            print_dev_info(&dev_info, DevType::PV);

            dev_infos.push(dev_info);
            disks.push(disk);
        }

        // fs label will be obtained later from the VM output
        let lv_info = DevInfo::lv(disk_path, None, vm_path)?;
        print_dev_info(&lv_info, DevType::LV);
        lv_info
    } else {
        let disk_paths: Vec<_> = disk_path.split(":").collect();

        for disk_path in disk_paths {
            if !Path::new(disk_path).exists() {
                return Err(anyhow!("disk {} not found", disk_path));
            }

            if mount_table.is_mounted(disk_path) {
                if config.allow_remount {
                    unmount_fs(Path::new(disk_path))?;
                    println!("Remounting with anylinuxfs...");
                } else {
                    return Err(anyhow!("{} is already mounted", disk_path));
                }
            }
            let di = DevInfo::pv(&disk_path)?;

            let disk = File::open(di.rdisk())?.acquire_lock(if config.read_only {
                FlockKind::Shared
            } else {
                FlockKind::Exclusive
            })?;

            print_dev_info(&di, DevType::Direct);

            dev_infos.push(di);
            disks.push(disk);
        }

        dev_infos[0].clone()
    };

    if let Some(fs_driver) = &config.fs_driver {
        mnt_dev_info.set_fs_driver(&fs_driver);
    };

    Ok((dev_infos, mnt_dev_info, disks))
}

fn ensure_enough_ram_for_luks(config: &mut Config) {
    if config.preferences.krun.ram_size_mib < 2560 {
        config.preferences.krun.ram_size_mib = 2560;
        println!(
            "Configured RAM size is lower than the minimum required for LUKS decryption, setting to {} MiB",
            config.preferences.krun.ram_size_mib
        );
    }
}

const DEFAULT_PACKAGES_DATA: &str = include_str!("../../init-rootfs/default-alpine-packages.txt");

fn get_default_packages() -> BTreeSet<String> {
    DEFAULT_PACKAGES_DATA
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect()
}

#[derive(Default)]
struct ServiceStatus {
    rpcbind_running: bool,
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
    fn run_shell(&mut self, cmd: ShellCmd) -> anyhow::Result<()> {
        let _lock_file = LockFile::new(LOCK_FILE)?.acquire_lock(FlockKind::Exclusive)?;
        let config = load_mount_config(cmd.mnt)?;

        if !cmd.skip_init {
            init_rootfs(&config.common, false)?;
        }

        let vm_env = if let Some(action) = config.get_action() {
            action.prepare_environment()?
        } else {
            vec![]
        };

        // host_println!("disk_path: {}", config.disk_path);
        host_println!("root_path: {}", config.common.root_path.display());
        host_println!("num_vcpus: {}", config.common.preferences.krun.num_vcpus);
        host_println!(
            "ram_size_mib: {}",
            config.common.preferences.krun.ram_size_mib
        );

        let (dev_info, _, _disks) = claim_devices(&config)?;

        let ctx = setup_vm(
            &config.common,
            dev_info.as_slice(),
            false,
            false,
            config.read_only,
        )
        .context("Failed to setup microVM")?;
        start_vmshell(ctx, cmd.command, &vm_env).context("Failed to start microVM shell")?;

        Ok(())
    }

    fn run_dmesg(&mut self) -> anyhow::Result<()> {
        let config = load_config(&CommonArgs::default())?;
        let kernel_log_path = config.root_path.join("kernel.log");

        if !kernel_log_path.exists() {
            return Ok(());
        }

        let log_file = File::open(kernel_log_path).context("Failed to open kernel.log file")?;
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
        Ok(())
    }

    fn run_apk(&mut self, cmd: ApkCmd) -> anyhow::Result<()> {
        let mut config = load_config(&CommonArgs::default())?;
        let config_file_path = &config.config_file_path;

        let alpine_config = &mut config.preferences.alpine;
        let default_packages = get_default_packages();

        let dns_server = dnsutil::get_dns_server_with_fallback();
        let vm_prelude = format!("echo nameserver {} > /etc/resolv.conf", dns_server);
        let apk_command = match cmd {
            ApkCmd::Info => {
                // Show information about custom packages
                for pkg in &alpine_config.custom_packages {
                    safe_println!("{}", pkg)?;
                }
                return Ok(());
            }
            ApkCmd::Add { mut packages } => {
                // remove default packages from the list
                packages.retain(|pkg| !default_packages.contains(pkg));

                // Add custom packages
                let mut package_set: BTreeSet<_> =
                    BTreeSet::from_iter(alpine_config.custom_packages.iter().cloned());
                package_set.extend(packages.iter().cloned());
                alpine_config.custom_packages = package_set.into_iter().collect();

                if packages.is_empty() {
                    // no-op
                    return Ok(());
                }

                format!("apk add {}", packages.join(" "))
            }
            ApkCmd::Del { mut packages } => {
                // remove default packages from the list
                packages.retain(|pkg| !default_packages.contains(pkg));

                // Remove custom packages
                alpine_config
                    .custom_packages
                    .retain(|pkg| !packages.contains(pkg));

                if packages.is_empty() {
                    // no-op
                    return Ok(());
                }

                format!("apk del {}", packages.join(" "))
            }
        };
        let vm_command = format!("{vm_prelude} && {apk_command}");

        let ctx = setup_vm(&config, &[], false, false, true).context("Failed to setup microVM")?;
        let status = start_vmshell_forked(ctx, Some(vm_command), &[])
            .context("Failed to start microVM shell")?;

        if status != 0 {
            return Err(anyhow::anyhow!(
                "microVM shell exited with status {}",
                status
            ));
        }
        // preferences are only saved if apk command was successful
        save_preferences(&config.preferences, config_file_path)?;

        Ok(())
    }

    fn run_rpcbind(&mut self, cmd: RpcBindCmd) -> anyhow::Result<()> {
        match cmd {
            RpcBindCmd::Register => rpcbind::services::register(),
            RpcBindCmd::Unregister => {
                rpcbind::services::unregister();
                Ok(())
            }
            RpcBindCmd::List => {
                let out = rpcbind::services::list()?;
                for entry in out {
                    safe_println!("{:?}", entry)?;
                }
                Ok(())
            }
        }
    }

    fn run_mount(&mut self, cmd: MountCmd) -> anyhow::Result<()> {
        let _lock_file = LockFile::new(LOCK_FILE)?.acquire_lock(FlockKind::Exclusive)?;
        let mut service_status = ServiceStatus::default();

        for port in [2049, 32765, 32767] {
            utils::check_port_availability([127, 0, 0, 1], port)?;
        }
        if let Err(e) = utils::try_port([0, 0, 0, 0], 111)
            && e.kind() == io::ErrorKind::AddrInUse
        {
            service_status.rpcbind_running = true;
        }

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
            let res = self.run_mount_child(config, service_status, forked.comm_fd());
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

    fn run_mount_child(
        &mut self,
        mut config: MountConfig,
        service_status: ServiceStatus,
        comm_write_fd: libc::c_int,
    ) -> anyhow::Result<()> {
        // pre-declare so it can be referenced in a deferred action
        let stdin_forwarder;
        let services_to_restore: Vec<_>;
        let mut deferred = Deferred::new();

        init_rootfs(&config.common, false)?;

        if !config.verbose {
            log::disable_console_log();
        }

        let vm_env = if let Some(action) = config.get_action() {
            action.prepare_environment()?
        } else {
            vec![]
        };

        // host_println!("disk_path: {}", config.disk_path);
        host_println!("root_path: {}", config.common.root_path.display());
        host_println!("num_vcpus: {}", config.common.preferences.krun.num_vcpus);
        host_println!(
            "ram_size_mib: {}",
            config.common.preferences.krun.ram_size_mib
        );

        let (dev_info, mut mnt_dev_info, _disks) = claim_devices(&config)?;

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

        let mut passphrase_callbacks = Vec::new();
        let mut passphrase_needed = false;
        for di in &dev_info {
            let is_luks = di.fs_type() == Some("crypto_LUKS");
            if is_luks || di.fs_type() == Some("BitLocker") {
                if is_luks {
                    ensure_enough_ram_for_luks(&mut config.common);
                }
                if config.common.passphrase_config == PassphrasePromptConfig::AskForEach {
                    let prompt_fn = diskutil::passphrase_prompt(Some(di.disk()));
                    passphrase_callbacks.push(prompt_fn);
                }
                passphrase_needed = true;
            }
        }
        if passphrase_needed && config.common.passphrase_config == PassphrasePromptConfig::OneForAll
        {
            let prompt_fn = diskutil::passphrase_prompt(None);
            passphrase_callbacks.push(prompt_fn);
        }

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

            let ctx = setup_vm(&config.common, &dev_info, true, true, config.read_only)
                .context("Failed to setup microVM")?;

            let to_decrypt: Vec<_> = iter::zip(dev_info.iter(), 'a'..='z')
                .filter_map(|(di, letter)| {
                    if di.fs_type() == Some("crypto_LUKS") || di.fs_type() == Some("BitLocker") {
                        Some(format!("/dev/vd{}", letter))
                    } else {
                        None
                    }
                })
                .collect();

            start_vmproxy(
                ctx,
                &config,
                &service_status,
                &vm_env,
                &mnt_dev_info,
                dev_info.len() > 1,
                to_decrypt,
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

            let rt_info = Arc::new(Mutex::new(api::RuntimeInfo {
                mount_config: config.clone(),
                dev_info: mnt_dev_info.clone(),
                session_pgid,
                vmm_pid: child_pid,
                gvproxy_pid,
                mount_point: None,
            }));

            api::serve_info(rt_info.clone());

            if service_status.rpcbind_running {
                services_to_restore = rpcbind::services::list()?
                    .into_iter()
                    .filter(|entry| {
                        entry.prog == rpcbind::RPCPROG_MNT
                            || entry.prog == rpcbind::RPCPROG_NFS
                            || entry.prog == rpcbind::RPCPROG_STAT
                    })
                    .collect();

                _ = deferred.add(|| {
                    rpcbind::services::unregister();
                    _ = rpcbind::services::rpcb_set_entries(&services_to_restore);
                });
                // if rpcbind is already running, we can use it to register our NFS server
                // but we have to unregister any conflicting system services first
                // (make sure to elevate if we need to unregister any services not owned by us)
                let unregister_fn = || -> anyhow::Result<()> {
                    let uid = config.common.invoker_uid;
                    if config.common.sudo_uid.is_none() && uid != 0 {
                        let any_root_svcs = services_to_restore.iter().any(|entry| {
                            Some(&entry.owner) != utils::user_name_from_uid(uid).as_ref()
                        });

                        if any_root_svcs {
                            safe_println!(
                                "rpcbind already running, need to use sudo for NFS setup"
                            )?;
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
                    rpcbind::services::register()
                        .context("Failed to register NFS server to rpcbind")?;
                }
            }

            let (nfs_ready_tx, nfs_ready_rx) = mpsc::channel();
            let (vm_pwd_prompt_tx, vm_pwd_prompt_rx) = mpsc::channel();

            let pty_fd = forked.master_fd();
            // Spawn a thread to read from the pipe
            _ = thread::spawn(move || {
                let mut nfs_ready = false;
                let mut fslabel: Option<String> = None;
                let mut fstype: Option<String> = None;
                let mut changed_to_ro = false;
                let mut exit_code = None;
                let mut buf_reader =
                    PassthroughBufReader::new(unsafe { File::from_raw_fd(pty_fd) });
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
                        // Notify the main thread that NFS server is ready
                        nfs_ready_tx
                            .send(NfsStatus::Ready(NfsReadyState {
                                fslabel: fslabel.take(),
                                fstype: fstype.take(),
                                changed_to_ro,
                                exports: exports.iter().cloned().collect(),
                            }))
                            .unwrap();
                        nfs_ready = true;
                    } else if line.starts_with("<anylinuxfs-exit-code") {
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
                    } else if line.starts_with("<anylinuxfs-label") {
                        fslabel = line.split(':').nth(1).map(|pattern| {
                            pattern
                                .trim()
                                .strip_suffix(">")
                                .unwrap_or(pattern)
                                .to_string()
                        })
                    } else if line.starts_with("<anylinuxfs-type") {
                        fstype = line.split(':').nth(1).map(|pattern| {
                            pattern
                                .trim()
                                .strip_suffix(">")
                                .unwrap_or(pattern)
                                .to_string()
                        })
                    } else if line.starts_with("<anylinuxfs-mount:changed-to-ro>") {
                        changed_to_ro = true;
                    } else if line.starts_with("<anylinuxfs-nfs-export") {
                        if let Some(export_path) = line.split(':').nth(1).map(|pattern| {
                            pattern
                                .trim()
                                .strip_suffix(">")
                                .unwrap_or(pattern)
                                .to_string()
                        }) {
                            exports.insert(export_path);
                        }
                    } else if line.starts_with("<anylinuxfs-passphrase-prompt:start>") {
                        vm_pwd_prompt_tx.send(true).unwrap();
                    } else if line.starts_with("<anylinuxfs-passphrase-prompt:end>") {
                        vm_pwd_prompt_tx.send(false).unwrap();
                    } else if !config.verbose && line.starts_with("<anylinuxfs-force-output:off>") {
                        log::disable_console_log();
                    } else if !config.verbose && line.starts_with("<anylinuxfs-force-output:on>") {
                        log::enable_console_log();
                    }

                    line.clear();
                }
                if !nfs_ready {
                    nfs_ready_tx.send(NfsStatus::Failed(exit_code)).unwrap();
                }
            });

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
                wait_for_nfs_server(2049, nfs_ready_rx).unwrap_or(NfsStatus::Failed(None));

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
                    _ = send_quit_cmd(&config.common);
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

                let share_name = mnt_dev_info.auto_mount_name();
                let share_path = match config.get_action() {
                    Some(action) if !action.override_nfs_export.is_empty() => {
                        action.override_nfs_export.clone()
                    }
                    _ => format!("/mnt/{share_name}"),
                };
                // let vers4 = mnt_dev_info.fs_type() == Some("zfs");
                let vers4 = false;
                if vers4 {
                    let mnt_point = PathBuf::from(format!("/Volumes/{share_name}"));
                    fs::create_dir_all(&mnt_point).with_context(|| {
                        format!("Failed to create mount point {}", mnt_point.display())
                    })?;

                    chown(
                        &mnt_point,
                        Some(config.common.invoker_uid),
                        Some(config.common.invoker_gid),
                    )
                    .with_context(|| {
                        format!("Failed to change owner of {}", mnt_point.display())
                    })?;

                    config.custom_mount_point = Some(mnt_point);
                }
                let mount_result = mount_nfs(&share_path, &config, vers4);
                match &mount_result {
                    Ok(_) => host_println!("Requested NFS share mount"),
                    Err(e) => host_eprintln!("Failed to request NFS mount: {:#}", e),
                };

                let mount_point_opt = if mount_result.is_ok() {
                    let nfs_path = PathBuf::from(format!("localhost:{}", share_path));
                    event_session.wait_for_mount(&nfs_path)
                } else {
                    None
                };

                if let Some(mount_point) = &mount_point_opt {
                    host_println!(
                        "{} was mounted as {}",
                        mnt_dev_info.disk(),
                        mount_point.display()
                    );

                    rt_info.lock().unwrap().mount_point = Some(mount_point.display().into());

                    let additional_exports = exports
                        .iter()
                        .map(|item| item.as_str())
                        .filter(|&export_path| export_path != &share_path);

                    let mnt_point_base = config
                        .custom_mount_point
                        .unwrap_or(PathBuf::from(format!("/Volumes/{share_name}")));
                    match mount_nfs_subdirs(&share_path, additional_exports, mnt_point_base) {
                        Ok(_) => {}
                        Err(e) => host_eprintln!("Failed to mount additional NFS exports: {:#}", e),
                    }
                }

                // drop privileges back to the original user if he used sudo
                drop_privileges(config.common.sudo_uid, config.common.sudo_gid)?;

                if can_detach {
                    // tell the parent to detach from console (i.e. exit)
                    unsafe { write_to_pipe(comm_write_fd, b"detach\n") }
                        .context("Failed to write to pipe")?;

                    // stop printing to the console
                    log::disable_console_log();

                    deferred.call_now(disable_stdin_fwd_action);
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
                send_quit_cmd(&config.common)?;
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

    fn run_mount_parent(
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

    fn run_unmount(&mut self, cmd: UnmountCmd) -> anyhow::Result<()> {
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

                println!("specified path: {:?}", &cmd.path);
                println!("mounted disk_path: {}", &rt_info.mount_config.disk_path);
                println!("mount_point: {}", mount_point.display());

                let mount_table = fsutil::MountTable::new()?;
                let our_mount_points: Vec<_> = mount_table
                    .mount_points()
                    .filter(|&mpt| {
                        mpt.to_string_lossy()
                            .starts_with(&*mount_point.to_string_lossy())
                    })
                    .collect();

                if !our_mount_points.is_empty() {
                    for mpt in our_mount_points {
                        host_println!("Unmounting {}", mpt.display());
                        // unmount_fs(Path::new(mpt))?;
                    }
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

    fn run_init(&mut self) -> anyhow::Result<()> {
        let _lock_file = LockFile::new(LOCK_FILE)?.acquire_lock(FlockKind::Exclusive)?;
        let config = load_config(&CommonArgs::default())?;
        init_rootfs(&config, true)?;

        Ok(())
    }

    fn run_config(&mut self, cmd: ConfigCmd) -> anyhow::Result<()> {
        let mut config = load_config(&cmd.common)?;
        let config_file_path = &config.config_file_path;

        let krun_config = &mut config.preferences.krun;

        if cmd == ConfigCmd::default() {
            println!("{}", &config.preferences);
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

        let misc_config = &mut config.preferences.misc;

        if let Some(pwd_cfg) = cmd.common.passphrase_config {
            misc_config.passphrase_config = pwd_cfg;
        }

        println!("{}", &config.preferences);

        save_preferences(&config.preferences, config_file_path)?;

        Ok(())
    }

    fn run_list(&mut self, cmd: ListCmd) -> anyhow::Result<()> {
        let mut config = load_config(&cmd.common)?;
        init_rootfs(&config, false)?;

        if cmd.decrypt.is_some() && !cmd.microsoft {
            ensure_enough_ram_for_luks(&mut config);
        }

        let labels = match cmd.microsoft {
            true => diskutil::WINDOWS_LABELS,
            false => diskutil::LINUX_LABELS,
        };
        println!(
            "{}",
            diskutil::list_partitions(config, cmd.decrypt.as_deref(), labels)?
        );
        Ok(())
    }

    fn run_log(&mut self, cmd: LogCmd) -> anyhow::Result<()> {
        let config = load_config(&CommonArgs::default())?;
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
                    .fs_driver()
                    .into_iter()
                    .chain(
                        rt_info
                            .mount_config
                            .mount_options
                            .iter()
                            .flat_map(|opts| opts.split(',')),
                    )
                    .collect();

                let user_name = utils::user_name_from_uid(rt_info.mount_config.common.invoker_uid)
                    .unwrap_or("<unknown>".into());

                println!(
                    "{} on {} ({}, mounted by {}) VM[cpus: {}, ram: {} MiB]",
                    &rt_info.mount_config.disk_path,
                    mount_point.display(),
                    info.join(", "),
                    &user_name,
                    rt_info.mount_config.common.preferences.krun.num_vcpus,
                    rt_info.mount_config.common.preferences.krun.ram_size_mib,
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
                    match validated_mount_point(&rt_info) {
                        MountStatus::Mounted(mount_point) => {
                            // try to trigger normal shutdown
                            println!("Unmounting {}...", mount_point.display());
                            unmount_fs(&mount_point)?;
                            return Ok(());
                        }
                        MountStatus::NotYet => {
                            let mut vm_exited_gracefully = false;
                            println!("Trying to shutdown anylinuxfs VM directly...");
                            if send_quit_cmd(&rt_info.mount_config.common).is_ok() {
                                // wait for vmm process to exit or become zombie
                                vm_exited_gracefully = wait_for_proc_exit(rt_info.vmm_pid).is_ok();
                            }
                            if !vm_exited_gracefully {
                                println!("Sending quit command didn't work, try SIGTERM...");
                                // not killing the whole process group, just the session leader;
                                // this should trigger graceful shutdown of the VMM and its parent
                                if unsafe { libc::kill(rt_info.session_pgid, libc::SIGTERM) } < 0 {
                                    return Err(io::Error::last_os_error())
                                        .context(format!("Failed to send SIGTERM to anylinuxfs"));
                                }
                            }
                            println!("VM exited gracefully");
                        }
                        MountStatus::NoLonger => {
                            println!("Already unmounted, shutting down...");
                            // not killing the whole process group, just the session leader;
                            // this should trigger graceful shutdown of the VMM and its parent
                            if unsafe { libc::kill(rt_info.session_pgid, libc::SIGTERM) } < 0 {
                                return Err(io::Error::last_os_error())
                                    .context(format!("Failed to send SIGTERM to anylinuxfs"));
                            }
                        }
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
            Commands::Unmount(cmd) => self.run_unmount(cmd),
            Commands::Init => self.run_init(),
            Commands::Status => self.run_status(),
            Commands::Log(cmd) => self.run_log(cmd),
            Commands::Config(cmd) => self.run_config(cmd),
            Commands::List(cmd) => self.run_list(cmd),
            Commands::Stop(cmd) => self.run_stop(cmd),
            Commands::Shell(cmd) => self.run_shell(cmd),
            Commands::Dmesg => self.run_dmesg(),
            Commands::Apk(cmd) => self.run_apk(cmd),
            Commands::Rpcbind(cmd) => self.run_rpcbind(cmd),
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
