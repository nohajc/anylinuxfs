use anyhow::Context;
use bstr::BString;
use common_utils::{OSType, PathExt, host_eprintln, host_println, log, safe_print, safe_println};

use cli::*;

use devinfo::DevInfo;
use nanoid::nanoid;
use toml_edit::{Document, DocumentMut};

use std::env;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

use notify::{RecursiveMode, Watcher};
use std::sync::mpsc;
use utils::{AcquireLock, FlockKind, LockFile, StatusError, write_to_pipe};

use crate::settings::{Config, ImageSource, KernelConfig, MountConfig, Preferences};

mod api;
#[allow(unused)]
mod bindings;
mod cli;
mod cmd_mount;
mod devinfo;
mod diskutil;
mod fsutil;
mod netutil;
mod pubsub;
mod rpcbind;
mod settings;
mod utils;
mod vm;
mod vm_image;
mod vm_network;

use cmd_mount::*;
use vm::*;

pub(crate) const LOCK_FILE: &str = "/tmp/anylinuxfs.lock";

pub(crate) fn to_exit_code(status: i32) -> i32 {
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

pub(crate) fn rand_string(len: usize) -> String {
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

/// Extracts the value from a `<tag:value>` line emitted by vmproxy.
/// Returns `None` if the line has no `:` separator.
pub(crate) fn parse_vm_tag_value(line: &str) -> Option<&str> {
    line.split_once(':').map(|(_, rest)| {
        let trimmed = rest.trim();
        trimmed.strip_suffix('>').unwrap_or(trimmed)
    })
}

pub(crate) fn is_read_only_set(mount_options: Option<&str>) -> bool {
    if let Some(options) = mount_options {
        options.split(',').any(|opt| opt == "ro")
    } else {
        false
    }
}

/// RAII guard that temporarily enables console logging in non-verbose mode.
/// When created (if `verbose` is false), enables console log.
/// When dropped, disables console log again.
/// If `verbose` is true, the guard is a no-op.
pub(crate) struct ConsoleLogGuard {
    active: bool,
}

impl ConsoleLogGuard {
    /// Temporarily re-enable console logging for a visible operation.
    /// When dropped, console logging will be disabled again.
    pub(crate) fn enable_temporarily(verbose: bool) -> Self {
        if !verbose {
            log::enable_console_log();
        }
        Self { active: !verbose }
    }
}

impl Drop for ConsoleLogGuard {
    fn drop(&mut self) {
        if self.active {
            log::disable_console_log();
        }
    }
}

pub(crate) fn default_linux_image_source(prefs: &impl Preferences) -> ImageSource {
    let default_name = prefs.default_image(OSType::Linux);
    prefs
        .images()
        .get(default_name)
        .map(|&s| s.to_owned())
        .unwrap_or(ImageSource::default())
}

fn load_config(common_args: &CommonArgs, debug_args: &DebugArgs) -> anyhow::Result<Config> {
    let sudo_uid = env::var("SUDO_UID")
        .map_err(anyhow::Error::from)
        .and_then(|s| Ok(s.parse::<libc::uid_t>()?))
        .ok();

    let sudo_gid = env::var("SUDO_GID")
        .map_err(anyhow::Error::from)
        .and_then(|s| Ok(s.parse::<libc::gid_t>()?))
        .ok();

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

    // Generate unique log file ID for this instance
    let log_file_id = rand_string(8);

    let profile_path = home_dir.join(".anylinuxfs");
    let config_file_path = home_dir.join(".anylinuxfs").join("config.toml");
    let log_dir = home_dir.join("Library").join("Logs");
    let log_file_path = log_dir.join(format!("anylinuxfs-{}.log", log_file_id));
    let kernel_log_file_path = log_dir.join(format!("anylinuxfs_kernel-{}.log", log_file_id));
    let nethelper_log_path = log_dir.join(format!("anylinuxfs_nethelper-{}.log", log_file_id));

    let libexec_path = prefix_dir.join("libexec");
    let init_rootfs_path = libexec_path.join("init-rootfs").to_owned();
    let kernel_path = libexec_path.join("Image").to_owned();
    let gvproxy_path = libexec_path.join("gvproxy").to_owned();
    let vmnet_helper_path = libexec_path.join("vmnet-helper").to_owned();
    let vmproxy_host_path = libexec_path.join("vmproxy").to_owned();

    let gvproxy_net_sock_path = format!("/tmp/network-{}.sock", rand_string(8));
    let vsock_path = format!("/tmp/anylinuxfs-{}-vsock", rand_string(8));
    let unixgram_sock_path = format!("/tmp/vfkit-{}.sock", rand_string(8));

    let global_prefix_dir = if prefix_dir.starts_with("/opt/homebrew") {
        PathBuf::from("/opt/homebrew")
    } else {
        prefix_dir.to_owned()
    };
    let global_cfg_path = global_prefix_dir.join("etc").join("anylinuxfs.toml");
    let all_cfg_paths = [global_cfg_path.as_path(), config_file_path.as_path()];
    let preferences = settings::load_preferences(all_cfg_paths.iter().cloned())?;

    // Root paths are derived from the configured default Linux image so that
    // a non-Alpine image defined in config is used from the start.
    let linux_src = default_linux_image_source(&preferences);
    let linux_base_path = profile_path.join(&linux_src.base_dir);
    let root_path = linux_base_path.join("rootfs");
    let root_ver_file_path = linux_base_path.join("rootfs.ver");

    let passphrase_config = common_args
        .passphrase_config
        .unwrap_or(preferences.passphrase_prompt_config());

    #[cfg(feature = "freebsd")]
    let zfs_os = common_args.zfs_os.unwrap_or(preferences.zfs_os());

    let net_helper = common_args
        .net_helper
        .unwrap_or(preferences.network_helper());

    let kernel = KernelConfig {
        os: OSType::Linux,
        path: kernel_path,
    };

    let rw_rootfs = debug_args.rw_rootfs;

    Ok(Config {
        home_dir,
        profile_path,
        exec_path,
        root_path,
        root_ver_file_path,
        config_file_path,
        log_file_path,
        kernel_log_file_path,
        libexec_path,
        init_rootfs_path,
        kernel,
        gvproxy_net_sock_path,
        gvproxy_path,
        nethelper_log_path,
        vmnet_helper_path,
        vmproxy_host_path,
        vsock_path,
        unixgram_sock_path,
        invoker_uid,
        invoker_gid,
        sudo_uid,
        sudo_gid,
        passphrase_config,
        rw_rootfs,
        #[cfg(feature = "freebsd")]
        zfs_os,
        net_helper,
        preferences,
    })
}

pub(crate) fn load_mount_config(cmd: MountCmd) -> anyhow::Result<MountConfig> {
    let common = load_config(&cmd.common, &cmd.debug)?;

    let disk_path = cmd.disk_ident();
    let mount_options = cmd.options;

    let mut nfs_options = cmd.nfs_options.unwrap_or_default();
    let nfs_export_opts = cmd.nfs_export_opts;
    let ignore_permissions = cmd.ignore_permissions;
    if ignore_permissions && !nfs_options.iter().any(|o| o == "noowners") {
        nfs_options.push("noowners".to_owned());
    }

    let allow_remount = cmd.remount;
    let custom_mount_point = match cmd.mount_point {
        Some(path) => {
            let path = fs::canonicalize(&path)
                .with_context(|| format!("Failed to resolve path {}", &path))?;

            if !fs::metadata(&path)
                .with_context(|| format!("Failed to get metadata for {}", &path.display()))?
                .is_dir()
            {
                anyhow::bail!("{} is not a directory", &path.display());
            }

            Some(path)
        }
        None => None,
    };

    let bind_addr = match cmd.bind_addr {
        Some(ref addr) => {
            let bind_addr = addr
                .parse()
                .with_context(|| format!("invalid IP address given: {}", addr))?;

            if bind_addr != Ipv4Addr::UNSPECIFIED
                && bind_addr != Ipv4Addr::LOCALHOST
                && bind_addr != Ipv6Addr::LOCALHOST
            {
                // check if the given bind address is assigned to any interface
                if let Ok(interfaces) = if_addrs::get_if_addrs() {
                    if !interfaces.iter().any(|iface| iface.ip() == bind_addr) {
                        anyhow::bail!(
                            "Bind address {} is not assigned to any interface",
                            bind_addr
                        );
                    }
                }
            }

            Some(bind_addr)
        }
        None => None,
    };

    let read_only = is_read_only_set(mount_options.as_deref());
    let verbose = cmd.verbose;

    let fs_driver = cmd.fs_driver;

    let open_finder = cmd.window;
    let kernel_page_size = cmd.kernel_page_size;

    let custom_action = if let Some(action_name) = cmd.action.as_deref() {
        match common.preferences.custom_actions().get(&action_name) {
            Some(_) => Some(action_name.to_owned()),
            None => {
                anyhow::bail!("unknown custom action: {}", action_name);
            }
        }
    } else {
        None
    };

    let key_file = cmd
        .key_file
        .clone()
        .or_else(|| env::var("ALFS_KEY_FILE").ok())
        .map(PathBuf::from)
        .map(|key_path| {
            if !key_path.exists() {
                anyhow::bail!("Key file not found: {}", key_path.display());
            }
            if !key_path.is_file() {
                anyhow::bail!("Key file path is not a file: {}", key_path.display());
            }
            Ok(key_path)
        })
        .transpose()?;

    // this is set dynamically later
    let assemble_raid = false;

    let vm_hostname: String = hostname_from_disk_ident(&disk_path)?;

    Ok(MountConfig {
        disk_path,
        read_only,
        mount_options,
        nfs_options,
        nfs_export_opts,
        ignore_permissions,
        allow_remount,
        vm_hostname,
        custom_mount_point,
        fs_driver,
        assemble_raid,
        bind_addr,
        verbose,
        open_finder,
        kernel_page_size,
        common,
        custom_action,
        key_file,
    })
}

pub(crate) fn hostname_from_disk_ident(disk_ident: &str) -> anyhow::Result<String> {
    let special = ["lvm", "raid"];

    let mut ident_iter = disk_ident.split(':');
    let mut first_token = ident_iter.next().unwrap();
    let mut prefix = String::new();

    if let Some(second_token) = ident_iter.next()
        && special.contains(&first_token)
    {
        prefix = first_token.into();
        first_token = second_token;
    }

    let disk_name = Path::new(first_token)
        .file_name()
        .unwrap_or(OsStr::new("disk"))
        .to_string_lossy();

    // Strip @s<digits> suffix from image partition identifiers
    let disk_name = if let Some(at_pos) = disk_name.rfind("@s") {
        let suffix = &disk_name[at_pos + 2..];
        if suffix.chars().all(|c| c.is_ascii_digit()) {
            &disk_name[..at_pos]
        } else {
            &disk_name
        }
    } else {
        &disk_name
    };

    let mut vm_hostname: String = disk_name
        .replace(
            |c| {
                matches!(
                    c,
                    ' ' | '_' | '\\' | '<' | '>' | '|' | '+' | ':' | '.' | ','
                )
            },
            "-",
        )
        .chars()
        .filter(|&c| c.is_ascii_alphanumeric() || c == '-')
        .collect();

    if vm_hostname.is_empty() {
        vm_hostname = "disk".to_string();
    }
    if !prefix.is_empty() {
        vm_hostname = format!("{}-{}", prefix, vm_hostname);
    }
    if vm_hostname.len() > 63 {
        vm_hostname.truncate(63);
    }

    Ok(vm_hostname)
}

pub(crate) fn drop_privileges(
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

pub(crate) fn drop_effective_privileges(
    sudo_uid: Option<libc::uid_t>,
    sudo_gid: Option<libc::gid_t>,
) -> anyhow::Result<()> {
    if let (Some(sudo_uid), Some(sudo_gid)) = (sudo_uid, sudo_gid) {
        if unsafe { libc::setegid(sudo_gid) } < 0 {
            return Err(io::Error::last_os_error()).context("Failed to setegid");
        }
        if unsafe { libc::seteuid(sudo_uid) } < 0 {
            return Err(io::Error::last_os_error()).context("Failed to seteuid");
        }
    }
    Ok(())
}

pub(crate) fn elevate_effective_privileges() -> anyhow::Result<()> {
    let real_uid = unsafe { libc::getuid() };
    let real_gid = unsafe { libc::getgid() };
    if unsafe { libc::seteuid(real_uid) } < 0 {
        return Err(io::Error::last_os_error()).context("Failed to seteuid");
    }
    if unsafe { libc::setegid(real_gid) } < 0 {
        return Err(io::Error::last_os_error()).context("Failed to setegid");
    }
    Ok(())
}

pub(crate) struct AppRunner {
    pub(crate) is_child: bool,
    pub(crate) print_log: bool,
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
        let _lock_file = LockFile::new(LOCK_FILE)?.acquire_lock(FlockKind::Shared)?;

        let config = load_mount_config(cmd.clone().into())?;
        #[cfg(feature = "freebsd")]
        let (mut config, src, root_disk_path) = match cmd.image {
            Some(image_name) => {
                let images = config.common.preferences.images();
                let matched_images: Vec<_> = images
                    .into_iter()
                    .filter_map(|(name, src)| name.starts_with(&image_name).then(|| src))
                    .collect();

                let src = match matched_images.len() {
                    2.. => {
                        anyhow::bail!("ambiguous image name: {}", image_name);
                    }
                    1 => matched_images[0],
                    0 => {
                        anyhow::bail!("unknown image: {}", image_name);
                    }
                };
                let freebsd_base_path = config.common.profile_path.join(&src.base_dir);
                let vm_disk_image = "freebsd-microvm-disk.img";
                let disk_path = freebsd_base_path.join(vm_disk_image);
                match src.os_type {
                    OSType::Linux => {
                        host_println!("root_path: {}", config.common.root_path.display());
                    }
                    OSType::FreeBSD => {
                        host_println!("root_disk: {}", disk_path.display());
                    }
                }

                (config.with_image_source(src), src.clone(), Some(disk_path))
            }
            None => {
                let default_src = default_linux_image_source(&config.common.preferences);
                host_println!("root_path: {}", config.common.root_path.display());
                (config.with_image_source(&default_src), default_src, None)
            }
        };
        #[cfg(not(feature = "freebsd"))]
        let src = default_linux_image_source(&config.common.preferences);

        if !cmd.skip_init {
            vm_image::init(&config.common, false, &src)?;
        }

        let (vm_env, _) = prepare_vm_environment(&config)?;

        host_println!("num_vcpus: {}", config.common.preferences.krun_num_vcpus());
        host_println!(
            "ram_size_mib: {}",
            config.common.preferences.krun_ram_size_mib()
        );

        #[allow(unused_mut)]
        let (mut dev_info, _, _disks) = claim_devices(&mut config)?;

        #[allow(unused_mut)]
        let mut opts = VMOpts::new()
            .read_only_disks(config.read_only)
            .read_only_root(!config.common.rw_rootfs);
        let os = config.common.kernel.os;

        #[cfg(feature = "freebsd")]
        if let Some(root_disk_path) = root_disk_path
            && os == OSType::FreeBSD
        {
            opts = opts.root_device("ufs:/dev/gpt/rootfs").legacy_console(true);
            dev_info = [DevInfo::pv(root_disk_path.as_bytes(), true)?]
                .iter()
                .chain(dev_info.iter())
                .cloned()
                .collect();
        }
        let net_mode = match cmd.no_tsi {
            true => NetworkMode::default_virtio_net(os, config.common.net_helper),
            false => NetworkMode::default_for_os(os),
        };
        // use_vsock must be true in virtio-net mode so the vsock device is present in the
        // guest; without it AF_VSOCK is unavailable, libkrun's clock_worker() in init fails
        // to bind its timesync socket and we get unreliable keyboard input in the shell
        let ctx = setup_vm(&config.common, &dev_info, net_mode, cmd.no_tsi, opts)
            .context("Failed to setup microVM")?;

        let vmproxy_bin: BString = match os {
            OSType::Linux => "/vmproxy",
            OSType::FreeBSD => "/vmproxy-bsd",
        }
        .into();
        let mut cmdline: Vec<BString> = vec![vmproxy_bin, "shell".into()];
        // In TSI mode the host provides the DNS server; in gvproxy/vmnet modes
        // the guest resolves it from its gateway so no --dns-server is needed.
        if os == OSType::Linux && !cmd.no_tsi {
            let dns_server = netutil::get_dns_server_with_fallback();
            cmdline.extend(["--dns-server".into(), dns_server.as_ref().into()]);
        }
        if let Some(command) = cmd.command.as_deref() {
            cmdline.extend(["--command".into(), command.into()]);
        }

        if os == OSType::Linux && !cmd.no_tsi {
            start_vm(&ctx, &cmdline, &vm_env).context("Failed to start microVM shell")?;
        } else {
            vm_image::setup_net_helper(&config.common, |cfg| {
                if let Some(cidr) = cfg.map(|c| c.vmnet_cidr) {
                    cmdline.extend(["-n".into(), cidr.to_string().into()]);
                }
                start_vm_forked(&ctx, &cmdline, &vm_env).context("Failed to start microVM shell")
            })?;
        }

        Ok(())
    }

    fn run_dmesg(&mut self) -> anyhow::Result<()> {
        let config = load_config(&CommonArgs::default(), &DebugArgs::default())?;
        let log_dir = config.home_dir.join("Library").join("Logs");

        // Find the most recently modified kernel log file (if it exists)
        let Some(kernel_log_path) = find_latest_log(&log_dir, "anylinuxfs_kernel-", ".log") else {
            return Ok(());
        };

        if !kernel_log_path.exists() {
            return Ok(());
        }

        let log_file = File::open(&kernel_log_path)
            .context(format!("Failed to open {}", kernel_log_path.display()))?;
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
        let _lock_file = LockFile::new(LOCK_FILE)?.acquire_lock(FlockKind::Exclusive)?;

        let mut config = load_config(&CommonArgs::default(), &DebugArgs::default())?;
        let config_file_path = &config.config_file_path;

        let alpine_packages = config.preferences.alpine_custom_packages();
        let default_packages = get_default_packages();

        let dns_server = netutil::get_dns_server_with_fallback();
        let vm_prelude = format!(
            "mount -t tmpfs tmpfs /tmp && echo nameserver {} > /tmp/resolv.conf",
            dns_server
        );
        let apk_command = match cmd {
            ApkCmd::Info => {
                // Show information about custom packages
                for pkg in alpine_packages {
                    safe_println!("{}", pkg)?;
                }
                return Ok(());
            }
            ApkCmd::Add { mut packages } => {
                // remove default packages from the list
                packages.retain(|pkg| !default_packages.contains(pkg));

                // Add custom packages
                let mut package_set = alpine_packages.clone();
                package_set.extend(packages.iter().map(|s| s.as_str()));
                config.preferences.user_mut().alpine.custom_packages =
                    package_set.into_iter().map(|s| s.to_owned()).collect();

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
                config
                    .preferences
                    .user_mut()
                    .alpine
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
        let cmdline: Vec<BString> = vec!["/bin/bash".into(), "-c".into(), vm_command.into()];

        let opts = VMOpts::new().read_only_disks(true).read_only_root(false);
        let ctx = setup_vm(&config, &[], NetworkMode::Default, false, opts)
            .context("Failed to setup microVM")?;
        let status =
            start_vm_forked(&ctx, &cmdline, &[]).context("Failed to start microVM shell")?;

        if status != 0 {
            anyhow::bail!("microVM shell exited with status {}", status);
        }
        // preferences are only saved if apk command was successful
        settings::save_preferences(config.preferences.user(), config_file_path)?;

        Ok(())
    }

    #[cfg(feature = "freebsd")]
    fn run_image(&mut self, cmd: ImageCmd) -> anyhow::Result<()> {
        let config = load_config(&CommonArgs::default(), &DebugArgs::default())?;
        let images = config.preferences.images();

        match cmd {
            ImageCmd::List { verbose } => {
                for (name, src) in images {
                    let suffix = if src.installed_in(&config.profile_path) {
                        " (installed)"
                    } else {
                        ""
                    };

                    let details = if verbose {
                        format!(": {:#?}", src)
                    } else {
                        "".to_string()
                    };
                    safe_println!("{}{}{}", name, suffix, details)?;
                }
            }
            ImageCmd::Install { name } => match images.get(name.as_str()) {
                Some(&src) => {
                    let _lock_file =
                        LockFile::new(LOCK_FILE)?.acquire_lock(FlockKind::Exclusive)?;
                    let config = config.with_image_source(src);
                    vm_image::init(&config, true, src)
                        .context(format!("Failed to install image {}", name))?;
                    safe_println!("Image {} installed successfully", name)?;
                }
                None => {
                    anyhow::bail!("unknown image {}", name);
                }
            },
            ImageCmd::Uninstall { name } => match images.get(name.as_str()) {
                Some(&src) => {
                    let _lock_file =
                        LockFile::new(LOCK_FILE)?.acquire_lock(FlockKind::Exclusive)?;
                    let config = config.with_image_source(src);
                    vm_image::remove(&config, src)
                        .context(format!("Failed to uninstall image {}", name))?;
                    safe_println!("Image {} uninstalled successfully", name)?;
                }
                None => {
                    anyhow::bail!("unknown image {}", name);
                }
            },
        }
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

    fn run_upgrade_config(&mut self, cmd: UpgradeConfigCmd) -> anyhow::Result<()> {
        let default_cfg_str = include_str!("../../etc/anylinuxfs.toml");
        let mut target_cfg = default_cfg_str
            .parse::<DocumentMut>()
            .context("Failed to parse default config")?;

        let input_path = Path::new(&cmd.input);
        if !input_path.exists() {
            let target_path = cmd.output.as_ref().map(Path::new).unwrap_or(input_path);

            // just write the default config and exit
            fs::write(target_path, default_cfg_str)
                .context("Failed to write default config file")?;
            return Ok(());
        }

        let current_cfg_str =
            fs::read_to_string(input_path).context("Failed to read current config file")?;
        let current_cfg = current_cfg_str
            .parse::<Document<String>>()
            .context("Failed to parse current config file")?;

        settings::merge_toml_configs(&mut target_cfg, &current_cfg)
            .context("Failed to merge config files")?;

        let target_cfg_str = target_cfg.to_string();
        match cmd.output {
            Some(output) => {
                fs::write(&output, target_cfg_str)
                    .context("Failed to write upgraded config file")?;
            }
            None => {
                safe_print!("{}", target_cfg_str)?;
            }
        }
        Ok(())
    }

    fn run_init(&mut self) -> anyhow::Result<()> {
        let _lock_file = LockFile::new(LOCK_FILE)?.acquire_lock(FlockKind::Exclusive)?;
        let config = load_config(&CommonArgs::default(), &DebugArgs::default())?;
        let src = default_linux_image_source(&config.preferences);
        vm_image::init(&config, true, &src)?;

        Ok(())
    }

    fn run_config(&mut self, cmd: ConfigCmd) -> anyhow::Result<()> {
        let mut config = load_config(&cmd.common, &DebugArgs::default())?;
        let config_file_path = &config.config_file_path;

        let krun_config = &mut config.preferences.user_mut().krun;

        if cmd == ConfigCmd::default() {
            println!("{}", &config.preferences.merged());
            return Ok(());
        }

        if let Some(log_level) = cmd.log_level {
            krun_config.set_log_level(log_level);
        }

        if let Some(num_vcpus) = cmd.num_vcpus {
            krun_config.num_vcpus = Some(num_vcpus);
        }
        if let Some(ram_size_mib) = cmd.ram_size_mib {
            krun_config.ram_size_mib = Some(ram_size_mib);
        }

        let misc_config = &mut config.preferences.user_mut().misc;
        if let Some(passphrase_config) = cmd.common.passphrase_config {
            misc_config.passphrase_config = Some(passphrase_config);
        }
        #[cfg(feature = "freebsd")]
        if let Some(zfs_os) = cmd.common.zfs_os {
            misc_config.zfs_os = Some(zfs_os);
        }

        let network_config = &mut config.preferences.user_mut().network;
        if let Some(net_helper) = cmd.common.net_helper {
            network_config.helper = Some(net_helper);
        }
        if let Some(vmnet_pool) = cmd.common.vmnet_pool {
            network_config.vmnet_pool = Some(vmnet_pool);
        }

        println!("{}", &config.preferences.merged());

        settings::save_preferences(config.preferences.user(), config_file_path)?;

        Ok(())
    }

    fn run_list(&mut self, cmd: ListCmd) -> anyhow::Result<()> {
        let _lock_file = LockFile::new(LOCK_FILE)?.acquire_lock(FlockKind::Shared)?;

        let mut config = load_config(&cmd.common, &cmd.debug)?;
        let linux_src = default_linux_image_source(&config.preferences);
        vm_image::init(&config, false, &linux_src)?;

        if cmd.decrypt.is_some() && !cmd.microsoft {
            ensure_enough_ram_for_luks(&mut config);
        }

        let mut labels = diskutil::ALL_LABELS;
        if cmd.linux {
            labels = diskutil::LINUX_LABELS;
        } else if cmd.microsoft {
            labels = diskutil::WINDOWS_LABELS;
        }

        let devices = cmd.disk.as_ref().map(|d| d.as_slice());

        println!(
            "{}",
            diskutil::list_partitions(config, devices, cmd.decrypt.as_deref(), labels)?
        );
        Ok(())
    }

    fn run_actions(&mut self) -> anyhow::Result<()> {
        let config = load_config(&CommonArgs::default(), &DebugArgs::default())?;
        for (action, config) in config.preferences.custom_actions() {
            safe_println!("{}: {}", action, config.description())?;
        }
        Ok(())
    }

    fn run_log(&mut self, cmd: LogCmd) -> anyhow::Result<()> {
        let config = load_config(&CommonArgs::default(), &DebugArgs::default())?;
        let log_dir = config.home_dir.join("Library").join("Logs");

        // Find the most recently modified log file
        let Some(log_file_path) = find_latest_log(&log_dir, "anylinuxfs-", ".log") else {
            return Ok(());
        };

        if !log_file_path.exists() {
            return Ok(());
        }

        let log_file = File::open(&log_file_path).context("Failed to open log file")?;
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
                .watch(&log_file_path, RecursiveMode::NonRecursive)
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
        let (active_instances, _) = collect_active_instances();

        if active_instances.is_empty() {
            return Ok(());
        }

        let mut status_list = Vec::new();
        for rt_info in active_instances {
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

            let user_name = utils::user_name_from_uid(rt_info.mount_config.common.invoker_uid)
                .unwrap_or("<unknown>".into());
            let mounted_by = format!("mounted by {}", &user_name);

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
                .chain([mounted_by.as_str()])
                .collect();

            let mut disk = rt_info.mount_config.disk_path.as_str();
            if disk.is_empty() {
                disk = "<unknown>";
            }
            status_list.push(format!(
                "{} on {} ({}) VM[cpus: {}, ram: {} MiB]",
                disk,
                mount_point.display(),
                info.join(", "),
                rt_info.mount_config.common.preferences.krun_num_vcpus(),
                rt_info.mount_config.common.preferences.krun_ram_size_mib(),
            ));
        }

        status_list.sort();
        for status in status_list {
            println!("{}", status);
        }

        Ok(())
    }

    fn run_stop(&mut self, cmd: StopCmd) -> anyhow::Result<()> {
        let (active_instances, _) = collect_active_instances();

        // If no path specified, require exactly one running instance
        if cmd.path.is_none() && active_instances.len() != 1 {
            if active_instances.is_empty() {
                anyhow::bail!("No anylinuxfs instance is currently running");
            }
            anyhow::bail!(
                "Multiple anylinuxfs instances are running; please specify the disk identifier or mount point."
            );
        }

        for rt_info in active_instances {
            // If a path was specified, check that this instance matches
            if let Some(target_path_str) = &cmd.path {
                let target_path = fs::canonicalize(target_path_str)
                    .unwrap_or_else(|_| PathBuf::from(target_path_str));
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

            // Found matching instance, now perform the stop
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
                        if send_quit_cmd(&rt_info.mount_config.common, rt_info.vm_native_ip).is_ok()
                        {
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
                        return Ok(());
                    }
                    MountStatus::NoLonger => {
                        println!("Already unmounted, shutting down...");
                        // not killing the whole process group, just the session leader;
                        // this should trigger graceful shutdown of the VMM and its parent
                        if unsafe { libc::kill(rt_info.session_pgid, libc::SIGTERM) } < 0 {
                            return Err(io::Error::last_os_error())
                                .context(format!("Failed to send SIGTERM to anylinuxfs"));
                        }
                        return Ok(());
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
                if send_quit_cmd(&rt_info.mount_config.common, rt_info.vm_native_ip).is_ok() {
                    // wait for vmm process to exit or become zombie
                    vm_exited_gracefully = wait_for_proc_exit(rt_info.vmm_pid).is_ok();
                }
                if vm_exited_gracefully {
                    println!("VM exited gracefully, killing the remaining processes...");
                } else {
                    println!("Killing anylinuxfs processes...");
                }
                if unsafe { libc::kill(rt_info.net_helper_pid, libc::SIGTERM) } == 0 {
                    // gvproxy could still terminate gracefully
                    if wait_for_proc_exit(rt_info.net_helper_pid).is_ok() {
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
                _ = vm_network::vsock_cleanup(&rt_info.mount_config.common.vsock_path);
                _ = vm_network::vfkit_sock_cleanup(&rt_info.mount_config.common.unixgram_sock_path);
                return Ok(());
            }
        }

        // No matching instance found
        anyhow::bail!(
            "No anylinuxfs instance found for: {}",
            cmd.path.as_deref().unwrap_or("<unknown>")
        )
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
            Commands::Actions => self.run_actions(),
            Commands::Stop(cmd) => self.run_stop(cmd),
            Commands::Shell(cmd) => self.run_shell(cmd),
            Commands::Dmesg => self.run_dmesg(),
            Commands::Apk(cmd) => self.run_apk(cmd),
            #[cfg(feature = "freebsd")]
            Commands::Image(cmd) => self.run_image(cmd),
            Commands::Rpcbind(cmd) => self.run_rpcbind(cmd),
            Commands::UpgradeConfig(cmd) => self.run_upgrade_config(cmd),
        }
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
