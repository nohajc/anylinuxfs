use clap::{ArgGroup, Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use common_utils::{NetHelper, OSType};
use ipnet::Ipv4Net;

use crate::settings::{KernelPage, KrunLogLevel, PassphrasePromptConfig};

#[derive(Parser)]
#[command(version, about, long_about = None)]
#[command(propagate_version = true)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub commands: Commands,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Mount a filesystem (the default if no command given)
    #[command(after_help = "Things you can mount:
- physical partitions
- raw disk images
- LVM or RAID volumes spanning one or more disks
- LUKS-encrypted partitions
- BitLocker-encrypted partitions
- LVM/RAID on LUKS
- multi-disk btrfs filesystems
- ZFS pools

Supported partition schemes:
- GPT
- MBR
- disk without partitions (single filesystem or LVM/LUKS container).

Recognized environment variables:
- ALFS_PASSPHRASE: passphrase for LUKS or BitLocker drive (optional)
- ALFS_PASSPHRASE1, ALFS_PASSPHRASE2, ...: passphrases for multiple drives if needed
- ALFS_KEY_FILE: path to a key file for unlocking encrypted drives")]
    Mount(MountCmd),
    /// Unmount a filesystem
    Unmount(UnmountCmd),
    /// Init Linux rootfs (can be used to reinitialize virtual environment)
    Init,
    /// Show status information (mount parameters, vm resources, etc.)
    Status,
    /// Show the latest application log (the rest is in ~/Library/Logs/)
    Log(LogCmd),
    /// Configure microVM parameters and other miscellaneous settings
    Config(ConfigCmd),
    /// List all available drives with compatible filesystems (run with sudo to get more detailed info)
    #[command(
        after_help = "Lists all partitions and LVM/RAID volumes. Can decrypt LUKS or BitLocker partition metadata too."
    )]
    List(ListCmd),
    /// List available custom actions
    Actions,
    /// Stop anylinuxfs (can be used if unresponsive)
    Stop(StopCmd),
    /// microVM shell for debugging (configures the VM according to mount options but only starts a shell)
    Shell(ShellCmd),
    /// Show the latest kernel log (after the VM has stopped)
    Dmesg,
    /// Manage custom alpine packages
    #[command(subcommand)]
    Apk(ApkCmd),
    /// Manage VM images
    #[cfg(feature = "freebsd")]
    #[command(subcommand)]
    Image(ImageCmd),
    #[command(subcommand, hide = true)]
    Rpcbind(RpcBindCmd),
    #[command(hide = true)]
    UpgradeConfig(UpgradeConfigCmd),
}

#[derive(Args, Default, PartialEq, Eq)]
pub(crate) struct CommonArgs {
    /// Passphrase configuration (ask for each drive / use one for all)
    #[arg(short, long)]
    pub passphrase_config: Option<PassphrasePromptConfig>,
    /// Operating system to use with ZFS
    #[cfg(feature = "freebsd")]
    #[arg(long)]
    pub zfs_os: Option<OSType>,
    /// Preferred network helper
    #[arg(long)]
    pub net_helper: Option<NetHelper>,
    /// IP address pool for vmnet (172.27.1.0/12 by default).
    #[arg(long)]
    pub vmnet_pool: Option<Ipv4Net>,
}

#[derive(Args, Default, Clone, PartialEq, Eq)]
pub(crate) struct DebugArgs {
    /// Use read-write root filesystem for the VM (only for troubleshooting)
    #[arg(long)]
    pub rw_rootfs: bool,
}

#[derive(Args, Clone)]
pub(crate) struct DiskIdentArg {
    #[cfg_attr(
        target_os = "macos",
        doc = "File path(s), LVM identifier or RAID identifier, e.g.:\n/dev/diskXsY[:/dev/diskYsZ:...]\npath/to/disk.img@s1[:path/to/disk2.img@s2:...]\nlvm:<vg-name>:diskXsY[:diskYsZ:...]:<lv-name>\nlvm:<vg-name>:path/to/disk.img@s1[:path/to/disk2.img@s2:...]:<lv-name>\nraid:diskXsY[:diskYsZ:...]\nraid:path/to/disk.img@s1[:path/to/disk2.img@s2:...]\n(see `list` command output for available volumes)"
    )]
    #[cfg_attr(
        not(target_os = "macos"),
        doc = "File path(s), LVM identifier or RAID identifier, e.g.:\n/dev/sdXN[:/dev/sdYM:...]\n/dev/nvmeXnYpZ[:/dev/nvmeAnBpC:...]\npath/to/disk.img@s1[:path/to/disk2.img@s2:...]\nlvm:<vg-name>:/dev/sdXN[:/dev/sdYM:...]:<lv-name>\nlvm:<vg-name>:path/to/disk.img@s1[:path/to/disk2.img@s2:...]:<lv-name>\nraid:/dev/sdXN[:/dev/sdYM:...]\nraid:path/to/disk.img@s1[:path/to/disk2.img@s2:...]\n(see `list` command output for available volumes)"
    )]
    #[clap(verbatim_doc_comment)]
    pub disk_ident: Option<String>,
}

#[derive(Args)]
pub(crate) struct MountCmd {
    #[command(flatten)]
    pub d: DiskIdentArg,
    #[cfg_attr(
        target_os = "macos",
        doc = "Custom mount path to override the default under /Volumes"
    )]
    #[cfg_attr(
        not(target_os = "macos"),
        doc = "Custom mount path to override the default under /mnt"
    )]
    pub mount_point: Option<String>,
    /// Options passed to the Linux mount command (comma-separated)
    #[arg(short, long)]
    pub options: Option<String>,
    /// NFS options passed to the host mount command (comma-separated)
    #[arg(short, long, value_delimiter = ',', num_args = 1..)]
    pub nfs_options: Option<Vec<String>>,
    /// Override **ALL** NFS export options for the mounted drive (/etc/exports in the VM),
    /// defaults to "{rw/ro},no_subtree_check,no_root_squash,insecure" when not specified
    #[clap(verbatim_doc_comment)]
    #[arg(long = "nfs-export-opts")]
    pub nfs_export_opts: Option<String>,
    /// Bypass Unix file permissions: files will appear to be owned by the current host user.
    #[arg(long = "ignore-permissions", conflicts_with = "nfs_export_opts")]
    pub ignore_permissions: bool,
    /// Allow remount: proceed even if the disk is already mounted by the host (NTFS, exFAT)
    #[arg(short, long)]
    pub remount: bool,
    /// Name of a custom action to perform after mounting (defined in config.toml)
    #[arg(short, long)]
    pub action: Option<String>,
    /// Filesystem driver override (e.g. for using ntfs3 instead of ntfs-3g)
    #[arg(short = 't', long = "type")]
    pub fs_driver: Option<String>,
    /// Path to a key file for unlocking encrypted drives (alternative to a passphrase)
    #[arg(short, long, conflicts_with = "passphrase_config")]
    pub key_file: Option<String>,
    #[command(flatten)]
    pub common: CommonArgs,
    /// Open Finder window with the mounted drive
    #[cfg(target_os = "macos")]
    #[arg(short, long, default_value = "true")]
    pub window: std::primitive::bool,
    /// Set this to share the mount to a different machine
    #[arg(short, long)]
    pub bind_addr: Option<String>,
    /// Linux kernel page size
    #[arg(long)]
    pub kernel_page_size: Option<KernelPage>,
    #[command(flatten)]
    pub debug: DebugArgs,
    #[arg(short, long)]
    pub verbose: bool,
}

impl MountCmd {
    pub fn disk_ident(&self) -> String {
        self.d.disk_ident.clone().unwrap_or_default()
    }
}

#[derive(Args)]
pub(crate) struct UnmountCmd {
    /// Disk identifier or mount point (unmounts all if not specified)
    #[arg(id = "DISK_IDENT|MOUNT_POINT")]
    pub path: Option<String>,
    /// Wait for VM to exit after unmounting
    #[arg(short, long)]
    pub wait_for_vm: bool,
}

#[derive(Args)]
pub(crate) struct LogCmd {
    /// Wait for additional logs to be appended
    #[arg(short, long)]
    pub follow: bool,
}

#[derive(Args, Default, PartialEq, Eq)]
pub(crate) struct ConfigCmd {
    /// Set krun log level
    #[arg(short, long)]
    pub log_level: Option<KrunLogLevel>,
    /// Set number of vCPUs
    #[arg(short, long)]
    pub num_vcpus: Option<u8>,
    /// Set RAM size in MiB
    #[arg(short, long)]
    pub ram_size_mib: Option<u32>,
    #[command(flatten)]
    pub common: CommonArgs,
}

#[derive(Args)]
#[clap(group(
    ArgGroup::new("part_types")
        .required(false)
        .args(&["linux", "microsoft"]),
))]
pub(crate) struct ListCmd {
    /// List only the specified disk(s) or image(s), delimited by space
    #[arg(value_name = "DISK|IMAGE", value_delimiter = ' ', num_args = 0..)]
    pub disk: Option<Vec<String>>,
    /// Decrypt LUKS partitions: comma-separated list of paths or "all"
    #[arg(short, long, value_delimiter = ',', num_args = 1..)]
    pub decrypt: Option<Vec<String>>,
    /// Only show Linux partitions
    #[arg(short, long)]
    pub linux: bool,
    /// Only show Microsoft partitions (NTFS, exFAT, ...)
    #[arg(short, long)]
    pub microsoft: bool,
    #[command(flatten)]
    pub common: CommonArgs,
    #[command(flatten)]
    pub debug: DebugArgs,
}

#[derive(Args)]
pub(crate) struct StopCmd {
    #[cfg_attr(
        target_os = "macos",
        doc = "Disk identifier or mount point to stop (e.g., /dev/diskXsY or /Volumes/MountPoint)"
    )]
    #[cfg_attr(
        not(target_os = "macos"),
        doc = "Disk identifier or mount point to stop (e.g., /dev/sda1 or /mnt/MountPoint)"
    )]
    pub path: Option<String>,
    /// Force stop the VM
    #[arg(short, long)]
    pub force: bool,
}

#[derive(Args, Clone)]
pub(crate) struct ShellCmd {
    /// Command to run in the shell
    #[arg(short, long)]
    pub command: Option<String>,
    /// Skip initialization of virtual environment
    #[arg(short, long)]
    pub skip_init: bool,
    #[cfg(feature = "freebsd")]
    /// OS image to use for the shell (alpine Linux by default)
    #[arg(short, long)]
    pub image: Option<String>,
    #[command(flatten)]
    pub d: DiskIdentArg,
    /// Allow remount: proceed even if the disk is already mounted by the host (NTFS, exFAT)
    #[arg(short, long)]
    pub remount: bool,
    /// Linux kernel page size
    #[arg(long)]
    pub kernel_page_size: Option<KernelPage>,
    /// Use virtio-net instead of TSI for Linux shell
    #[arg(long)]
    pub no_tsi: bool,
    #[command(flatten)]
    pub debug: DebugArgs,
}

impl From<ShellCmd> for MountCmd {
    fn from(shell_cmd: ShellCmd) -> Self {
        MountCmd {
            d: shell_cmd.d,
            mount_point: None,
            options: None,
            nfs_options: None,
            nfs_export_opts: None,
            ignore_permissions: false,
            remount: shell_cmd.remount,
            action: None,
            fs_driver: None,
            common: CommonArgs::default(),
            #[cfg(target_os = "macos")]
            window: false,
            bind_addr: None,
            kernel_page_size: shell_cmd.kernel_page_size,
            debug: shell_cmd.debug,
            verbose: false,
            key_file: None,
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum ApkCmd {
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
pub(crate) enum ImageCmd {
    /// List available VM images
    List {
        #[arg(short, long)]
        verbose: bool,
    },
    /// Install a VM image (installing alpine-latest is equivalent to `anylinuxfs init`)
    Install {
        /// VM image name
        name: String,
    },
    /// Uninstall a VM image
    Uninstall {
        /// VM image name
        name: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum RpcBindCmd {
    /// Register RPC services
    Register,
    /// Unregister RPC services
    Unregister,
    /// List registered RPC services
    List,
}

#[derive(Args)]
pub(crate) struct UpgradeConfigCmd {
    pub input: String,
    #[arg(short, long)]
    pub output: Option<String>,
}

#[derive(Parser)]
#[command(version, about = "Mount a filesystem (the default if no command given)", long_about = None)]
pub(crate) struct CliMount {
    #[command(flatten)]
    pub cmd: MountCmd,
}

pub(crate) trait TryParseCommand<T: FromArgMatches> {
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
    pub fn try_parse_with_default_cmd() -> Result<Cli, clap::Error> {
        let mount_cmd_usage =
            "\x1b[1manylinuxfs [mount]\x1b[0m [OPTIONS] [DISK_IDENT] [MOUNT_POINT]";
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
