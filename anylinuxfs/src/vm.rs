use anyhow::Context;
use bstr::BString;
use common_utils::{Deferred, FromPath, NetHelper, OSType, host_eprintln, host_println};
use ipnet::Ipv4Net;
use krun as bindings;
use serde::Serialize;
use serde_with::{DisplayFromStr, serde_as};

use std::collections::HashSet;
use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::chown;
use std::path::{Path, PathBuf};
use std::ptr::null;
use std::sync::Once;

use crate::ResultWithCtx;
use crate::cmd_mount::NetworkEnv;
use crate::devinfo::DevInfo;
use crate::settings::{Config, MountConfig, PassphrasePromptConfig, Preferences};
use crate::utils::{HasPipeInFd, HasPipeOutFds};
use crate::vm_image::{self, IsoAdd};
use crate::vm_network;
use crate::{rand_string, to_exit_code, utils};

pub(crate) struct VMOpts {
    add_disks_ro: bool,
    root_device: Option<String>,
    read_only_root: bool,
    #[cfg(feature = "freebsd")]
    legacy_console: bool,
}

impl VMOpts {
    pub(crate) fn new() -> Self {
        Self {
            add_disks_ro: false,
            root_device: None,
            read_only_root: true,
            #[cfg(feature = "freebsd")]
            legacy_console: false,
        }
    }

    pub(crate) fn read_only_disks(mut self, value: bool) -> Self {
        self.add_disks_ro = value;
        self
    }

    pub(crate) fn read_only_root(mut self, value: bool) -> Self {
        self.read_only_root = value;
        self
    }

    #[cfg(feature = "freebsd")]
    pub(crate) fn root_device(mut self, device: impl AsRef<str>) -> Self {
        self.root_device = Some(device.as_ref().to_owned());
        self
    }

    #[cfg(feature = "freebsd")]
    pub(crate) fn legacy_console(mut self, value: bool) -> Self {
        self.legacy_console = value;
        self
    }
}

impl Default for VMOpts {
    fn default() -> Self {
        Self::new()
    }
}

static KRUN_LOG_LEVEL_INIT: Once = Once::new();

#[derive(Debug, Clone)]
pub(crate) struct VMContext {
    id: u32,
    os: OSType,
    root_path: Option<PathBuf>,
    invoker_uid: libc::uid_t,
    invoker_gid: libc::gid_t,
    sudo_uid: Option<libc::uid_t>,
    sudo_gid: Option<libc::gid_t>,
    vmnet_cidr: Option<Ipv4Net>,
}

impl VMContext {
    pub(crate) fn set_vmnet_cidr(&mut self, cidr: Option<Ipv4Net>) {
        self.vmnet_cidr = cidr;
    }

    pub(crate) fn sudo_uid(&self) -> Option<libc::uid_t> {
        self.sudo_uid
    }

    pub(crate) fn sudo_gid(&self) -> Option<libc::gid_t> {
        self.sudo_gid
    }
}

pub(crate) enum NetworkMode {
    Default,
    GvProxy,
    VmNet,
}

impl NetworkMode {
    pub(crate) fn default_for_os(os: OSType) -> Self {
        match os {
            OSType::FreeBSD => NetworkMode::GvProxy,
            OSType::Linux => NetworkMode::Default,
        }
    }

    pub(crate) fn default_virtio_net(os: OSType, net_helper: NetHelper) -> Self {
        match os {
            OSType::FreeBSD => NetworkMode::GvProxy,
            OSType::Linux => match net_helper {
                NetHelper::GvProxy => NetworkMode::GvProxy,
                NetHelper::VmNet => NetworkMode::VmNet,
            },
        }
    }
}

/// Taken from https://github.com/containers/libkrun/blob/7116644749c7b1028a970c9e8bd2d0163745a225/include/libkrun.h#L269
// const NET_FEATURE_CSUM: u32 = 1 << 0;
// const NET_FEATURE_GUEST_CSUM: u32 = 1 << 1;
// const NET_FEATURE_GUEST_TSO4: u32 = 1 << 7;
// const NET_FEATURE_GUEST_TSO6: u32 = 1 << 8;
// const NET_FEATURE_GUEST_UFO: u32 = 1 << 10;
// const NET_FEATURE_HOST_TSO4: u32 = 1 << 11;
// const NET_FEATURE_HOST_TSO6: u32 = 1 << 12;
// const NET_FEATURE_HOST_UFO: u32 = 1 << 14;

/// These are the features enabled by krun_set_passt_fd and krun_set_gvproxy_path.
// const COMPAT_NET_FEATURES: u32 = NET_FEATURE_CSUM
//     | NET_FEATURE_GUEST_CSUM
//     | NET_FEATURE_GUEST_TSO4
//     | NET_FEATURE_GUEST_UFO
//     | NET_FEATURE_HOST_TSO4
//     | NET_FEATURE_HOST_UFO;

pub(crate) fn setup_vm(
    config: &Config,
    dev_info: &[DevInfo],
    net_mode: NetworkMode,
    use_vsock: bool,
    opts: VMOpts,
) -> anyhow::Result<VMContext> {
    let ctx_id = bindings::krun_create_ctx().context("Failed to create context")?;

    let level = config.preferences.krun_log_level_numeric();
    KRUN_LOG_LEVEL_INIT.call_once(|| {
        _ = bindings::krun_set_log_level(level);
    });

    let num_vcpus = config.preferences.krun_num_vcpus();
    let ram_mib = config.preferences.krun_ram_size_mib();
    bindings::krun_set_vm_config(ctx_id, num_vcpus, ram_mib).context("Failed to set VM config")?;

    #[cfg(feature = "freebsd")]
    if opts.legacy_console {
        bindings::krun_disable_implicit_console(ctx_id)
            .context("Failed to disable implicit console")?;
        unsafe { bindings::krun_add_serial_console_default(ctx_id, 0, 1) }
            .context("Failed to add serial console")?;
    }

    // run vmm as the original user if he used sudo.
    // Skip on Linux: libkrun's internal setuid() drops supplementary groups
    // needed for /dev/kvm and /dev/vhost-* access, and root inside libkrun is
    // fine on Linux (mirrors how libkrun's own tests run — they don't escalate).
    #[cfg(target_os = "macos")]
    if let Some(uid) = config.sudo_uid {
        bindings::krun_setuid(ctx_id, uid).context("Failed to set vmm uid")?;
    }

    #[cfg(target_os = "macos")]
    if let Some(gid) = config.sudo_gid {
        bindings::krun_setgid(ctx_id, gid).context("Failed to set vmm gid")?;
    }

    if opts.root_device.is_none() {
        unsafe { bindings::krun_set_root(ctx_id, CString::from_path(&config.root_path).as_ptr()) }
            .context("Failed to set root")?;
    }

    for (i, di) in dev_info.iter().enumerate() {
        unsafe {
            bindings::krun_add_disk(
                ctx_id,
                CString::new(format!("data{}", i)).unwrap().as_ptr(),
                CString::from_path(di.rdisk()).as_ptr(),
                opts.add_disks_ro,
            )
        }
        .context("Failed to add disk")?;
    }

    match net_mode {
        NetworkMode::GvProxy => {
            unsafe {
                bindings::krun_set_gvproxy_path(
                    ctx_id,
                    CString::new(config.unixgram_sock_path.as_str())
                        .unwrap()
                        .as_ptr(),
                )
            }
            .context("Failed to set gvproxy path")?;
        }
        NetworkMode::VmNet => {
            unsafe {
                bindings::krun_add_net_unixgram(
                    ctx_id,
                    CString::from_path(&config.unixgram_sock_path).as_ptr(),
                    -1,
                    vm_network::random_mac_address().as_ptr(),
                    0, // COMPAT_NET_FEATURES,
                    0,
                )
            }
            .context("Failed to add vmnet socket")?;
        }
        NetworkMode::Default => (),
    };

    vm_network::vsock_cleanup(&config.vsock_path)?;

    if use_vsock {
        unsafe {
            bindings::krun_add_vsock_port2(
                ctx_id,
                12700,
                CString::new(config.vsock_path.as_str()).unwrap().as_ptr(),
                true,
            )
        }
        .context("Failed to add vsock port")?;
    }

    unsafe { bindings::krun_set_workdir(ctx_id, c"/".as_ptr()) }
        .context("Failed to set workdir")?;

    let os = config.kernel.os;
    let cmdline = &CString::new(match os {
        OSType::Linux => {
            format!(
                "reboot=k panic=-1 panic_print=0 console=hvc0 rootfstype=virtiofs {} quiet no-kvmapf init=/init.krun",
                if opts.read_only_root { "ro" } else { "rw" }
            )
        }
        OSType::FreeBSD => match opts.root_device {
            Some(root_device) => {
                format!(
                    "FreeBSD:vfs.root.mountfrom={root_device} \
                        -mq init_path=/init-freebsd"
                )
            }
            None => {
                anyhow::bail!("root device must be specified for FreeBSD");
            }
        },
    }).unwrap();

    unsafe {
        bindings::krun_set_kernel(
            ctx_id,
            CString::from_path(&config.kernel.path).as_ptr(),
            0, // KRUN_KERNEL_FORMAT_RAW
            null(),
            cmdline.as_ptr(),
        )
    }
    .context("Failed to set kernel")?;

    let root_path = match os {
        OSType::Linux => Some(config.root_path.clone()),
        OSType::FreeBSD => None, // no virtiofs => no root path
    };

    let invoker_uid = config.invoker_uid;
    let invoker_gid = config.invoker_gid;

    Ok(VMContext {
        id: ctx_id,
        os,
        root_path,
        invoker_uid,
        invoker_gid,
        sudo_uid: config.sudo_uid,
        sudo_gid: config.sudo_gid,
        vmnet_cidr: None,
    })
}

/// Key file information prepared for transfer into the VM, created in the parent process
/// before forking. The ISO fd (FreeBSD) is inherited by the child via fork.
pub(crate) struct PreparedKeyFile {
    /// Extra CLI args to pass to vmproxy (e.g. ["--key-file", "/.alfs_keyfile"])
    args: Vec<BString>,
    /// For FreeBSD: the open ISO file whose fd is inherited by the child.
    /// `krun_add_disk` is called in the child using `/dev/fd/{iso_fd}`.
    iso_file: Option<File>,
}

impl PreparedKeyFile {
    fn none() -> Self {
        Self {
            args: vec![],
            iso_file: None,
        }
    }
}

/// Prepare the key file for transfer into the VM. Must be called in the parent
/// process before forking.
///
/// Linux: copies the key file into the virtiofs-mapped rootfs dir. The `deferred`
/// parameter is used to register cleanup (removal of the copied file) that runs in
/// the parent after the child exits.
///
/// FreeBSD: creates an ISO containing the key file, opens it by fd, then immediately
/// removes the temp dir. The open fd (stored in `PreparedKeyFile`) keeps the ISO
/// accessible via `/dev/fd/<N>` until process termination — same trick as
/// `set_vm_cmdline`.
pub(crate) fn prepare_key_file_for_vm(
    key_file: Option<&Path>,
    os: OSType,
    config: &Config,
    deferred: &mut Deferred,
) -> anyhow::Result<PreparedKeyFile> {
    let Some(key_file_host_path) = key_file else {
        return Ok(PreparedKeyFile::none());
    };

    match os {
        OSType::Linux => {
            // Copy the key file into the virtiofs-mapped rootfs directory.
            // The VM sees it as /.alfs_keyfile via virtiofs.
            let keyfile_name = format!(".alfs_keyfile-{}", rand_string(8));
            let dst = config.root_path.join(&keyfile_name);
            fs::copy(key_file_host_path, &dst)
                .with_context(|| format!("Failed to copy key file to rootfs: {}", dst.display()))?;
            chown(&dst, Some(config.invoker_uid), Some(config.invoker_gid))
                .with_context(|| format!("Failed to change owner of {}", dst.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&dst, fs::Permissions::from_mode(0o600))
                    .context("Failed to set permissions on key file in rootfs")?;
            }
            // Register cleanup in the parent's Deferred — runs after the child exits.

            deferred.add(move || {
                if let Err(e) = fs::remove_file(&dst) {
                    host_eprintln!(
                        "Warning: failed to remove key file from rootfs {}: {:#}",
                        dst.display(),
                        e
                    );
                }
            });
            Ok(PreparedKeyFile {
                args: vec!["--key-file".into(), format!("/{}", keyfile_name).into()],
                iso_file: None,
            })
        }
        OSType::FreeBSD => {
            // Pack the key file into an ISO image; the ISO is attached as a read-only
            // disk to the VM. The child inherits the open fd via fork.
            let tmp_dir = PathBuf::from("/tmp").join(format!("alfs-kf-{}", rand_string(8)));
            fs::create_dir_all(&tmp_dir).context("Failed to create key file temp directory")?;

            let iso_keyfile_name = "keyfile";
            let key_dst = tmp_dir.join(iso_keyfile_name);
            fs::copy(key_file_host_path, &key_dst).with_context(|| {
                format!("Failed to copy key file to temp dir: {}", key_dst.display())
            })?;

            let iso_path = tmp_dir.join("keyfile.iso");
            vm_image::create_iso(
                &iso_path,
                &tmp_dir,
                &tmp_dir,
                IsoAdd::Files(&[iso_keyfile_name]),
                Some("ALFS_KEYFILE"),
            )
            .context("Failed to create ISO image for key file")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&iso_path, fs::Permissions::from_mode(0o600))
                    .context("Failed to set permissions on key file ISO")?;
            }

            // Open the ISO by fd before deleting the temp dir; this way the ISO
            // remains accessible via /dev/fd/<N> until process termination — the
            // same trick used in set_vm_cmdline for the krun config ISO.
            let iso_file = File::open(&iso_path).context("Failed to open key file ISO")?;

            // All staging files can be removed immediately; the open fd is enough.
            fs::remove_dir_all(&tmp_dir).with_context(|| {
                format!(
                    "Failed to remove key file temp directory {}",
                    tmp_dir.display()
                )
            })?;

            Ok(PreparedKeyFile {
                args: vec![],
                iso_file: Some(iso_file),
            })
        }
    }
}

pub(crate) fn start_vmproxy(
    ctx: &VMContext,
    config: &MountConfig,
    network_env: &NetworkEnv,
    env: &[BString],
    dev_info: &DevInfo,
    multi_device: bool,
    to_decrypt: Vec<String>,
    prepared_key_file: &PreparedKeyFile,
    before_start: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let to_decrypt_arg = if to_decrypt.is_empty() {
        None
    } else {
        Some(to_decrypt.join(","))
    };

    let reuse_passphrase = config.common.passphrase_config == PassphrasePromptConfig::OneForAll;

    let vmproxy: BString = match config.common.kernel.os {
        OSType::Linux => "/vmproxy",
        OSType::FreeBSD => "/vmproxy-bsd",
    }
    .into();

    let mount_name = match config.custom_mount_name() {
        Some(name) => name.as_bytes().into(),
        None => dev_info.auto_mount_name(),
    };

    let custom_mount_point = config.custom_mount_point.is_some();
    let assemble_raid = config.assemble_raid;

    let mut bind_addrs = HashSet::new();

    if let Some(addr) = config.bind_addr {
        bind_addrs.insert(addr.to_string());
    }

    if let Some(addr) = network_env.usable_loopback_ip.as_ref() {
        bind_addrs.insert(addr.to_string());
    }

    let args: Vec<_> = [
        vmproxy,
        "mount".into(),
        dev_info.vm_path().into(),
        mount_name,
        "-b".into(),
        Vec::from_iter(bind_addrs).join(",").into(),
    ]
    .into_iter()
    .chain(custom_mount_point.then_some("-c".into()).into_iter())
    .chain(
        ctx.vmnet_cidr
            .as_ref()
            .into_iter()
            .flat_map(|cidr| ["-n".into(), cidr.to_string().into()]),
    )
    .chain(["-t".into(), dev_info.fs_type().unwrap_or("auto").into()])
    .chain(
        assemble_raid
            .then_some("--assemble-raid".into())
            .into_iter(),
    )
    .chain(
        config
            .fs_driver
            .as_deref()
            .into_iter()
            .flat_map(|fs_driver| ["--fs-driver".into(), fs_driver.into()]),
    )
    .chain(
        config
            .mount_options
            .as_deref()
            .into_iter()
            .flat_map(|opts| ["-o".into(), opts.into()]),
    )
    .chain(
        to_decrypt_arg
            .as_deref()
            .into_iter()
            .flat_map(|d| vec!["-d".into(), d.into()]),
    )
    .chain(config.get_action().into_iter().flat_map(|action| {
        vec![
            "-a".into(),
            action
                .percent_encode()
                .expect("failed to serialize action")
                .into(),
        ]
    }))
    .chain(
        config
            .nfs_export_opts
            .as_deref()
            .into_iter()
            .flat_map(|opts| ["--nfs-export-opts".into(), opts.into()]),
    )
    .chain(multi_device.then_some("-m".into()).into_iter())
    .chain(reuse_passphrase.then_some("-r".into()).into_iter())
    .chain(
        network_env
            .rpcbind_running
            .then_some("-h".into())
            .into_iter(),
    )
    .chain(config.verbose.then_some("-v".into()).into_iter())
    .chain(
        config
            .ignore_permissions
            .then_some("--ignore-permissions".into())
            .into_iter(),
    )
    .chain(prepared_key_file.args.iter().cloned())
    .collect();

    // For FreeBSD: attach the key file ISO disk using the fd inherited from the parent.
    if let Some(iso_file) = &prepared_key_file.iso_file {
        let iso_fd = iso_file.as_raw_fd();
        let iso_fd_path = format!("/dev/fd/{}", iso_fd);
        unsafe {
            bindings::krun_add_disk(
                ctx.id,
                CString::new("keyfile").unwrap().as_ptr(),
                CString::new(iso_fd_path.as_str()).unwrap().as_ptr(),
                true,
            )
        }
        .context("Failed to attach key file disk to VM")?;
    }

    host_println!("vmproxy args: {:?}", &args);
    set_vm_cmdline(ctx, &args, env)?;

    raise_nofile_limit();
    before_start().context("Before start callback failed")?;
    #[cfg(not(target_os = "macos"))]
    crate::install_invoker_supplementary_groups(ctx.sudo_uid(), ctx.sudo_gid())?;
    bindings::krun_start_enter(ctx.id).context("Failed to start VM")?;

    Ok(())
}

#[serde_as]
#[derive(Serialize)]
struct KrunConfigProcess<'a, 'b> {
    #[serde_as(as = "[DisplayFromStr]")]
    args: &'a [BString],
    #[serde_as(as = "[DisplayFromStr]")]
    env: &'b [BString],
}

#[derive(Serialize)]
struct KrunConfig<'a, 'b> {
    process: KrunConfigProcess<'a, 'b>,
}

pub(crate) fn set_vm_cmdline(
    ctx: &VMContext,
    args: &[BString],
    env: &[BString],
) -> anyhow::Result<()> {
    let krun_config_tmp_dir;
    let mut deferred = Deferred::new();

    let krun_config = serde_json::to_string(&KrunConfig {
        process: KrunConfigProcess { args, env },
    })
    .context("Failed to serialize krun config")?;

    match ctx.os {
        OSType::Linux => {
            let krun_config_file_name = ".krun_config.json";
            let krun_config_file = ctx.root_path.as_ref().unwrap().join(krun_config_file_name);
            fs::write(&krun_config_file, krun_config.as_bytes()).with_context(|| {
                format!(
                    "Failed to write krun config file {}",
                    krun_config_file.display()
                )
            })?;
            chown(
                &krun_config_file,
                Some(ctx.invoker_uid),
                Some(ctx.invoker_gid),
            )
            .with_context(|| format!("Failed to change owner of {}", krun_config_file.display()))?;
        }
        OSType::FreeBSD => {
            krun_config_tmp_dir = PathBuf::from("/tmp").join(format!("alfs-{}", rand_string(8)));
            fs::create_dir_all(&krun_config_tmp_dir)
                .context("Failed to create krun config temp directory")?;

            deferred.add(|| {
                _ = fs::remove_dir_all(&krun_config_tmp_dir);
            });

            let krun_config_file_name = "krun_config.json";
            let krun_config_file = krun_config_tmp_dir.join(krun_config_file_name);
            fs::write(&krun_config_file, krun_config.as_bytes()).with_context(|| {
                format!(
                    "Failed to write krun config file {}",
                    krun_config_file.display()
                )
            })?;

            let krun_config_iso = krun_config_tmp_dir.join("krun_config.iso");
            vm_image::create_iso(
                &krun_config_iso,
                &krun_config_tmp_dir,
                &krun_config_tmp_dir,
                IsoAdd::Files(&[&krun_config_file_name]),
                Some("KRUN_CONFIG"),
            )
            .context("Failed to create ISO image for krun config")?;

            // open the file before we unlink the temp dir and everything inside it;
            // this way the config iso remains accessible until process termination
            let config_iso_fd = File::open(&krun_config_iso)?.into_raw_fd();
            let krun_config_iso_fd_path = format!("/dev/fd/{}", config_iso_fd);

            unsafe {
                bindings::krun_add_disk(
                    ctx.id,
                    CString::new("config").unwrap().as_ptr(),
                    CString::from_path(&krun_config_iso_fd_path).as_ptr(),
                    true,
                )
            }
            .context("Failed to add disk")?;
        }
    }

    Ok(())
}

pub(crate) fn start_vm_forked(
    ctx: &VMContext,
    cmdline: &[BString],
    env: &[BString],
) -> anyhow::Result<i32> {
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error()).context("Failed to fork process");
    } else if pid == 0 {
        // Child process
        start_vm(ctx, cmdline, env)?;
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

const RLIMIT: libc::rlim_t = 16384;

/// Raise the soft RLIMIT_NOFILE so that libkrun has enough file descriptors
/// to set up virtiofs, virtio-net, serial console pipes, etc.
/// On macOS the default soft limit is 256, but libkrun typically needs ~300+
/// fds for a fully mounted VM (149 virtiofs pipes alone for the Alpine rootfs).
fn raise_nofile_limit() {
    let mut rl = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) } == 0 && rl.rlim_cur < RLIMIT {
        rl.rlim_cur = RLIMIT;
        unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &rl) };
    }
}

pub(crate) fn start_vm(
    ctx: &VMContext,
    cmdline: &[BString],
    env: &[BString],
) -> anyhow::Result<()> {
    set_vm_cmdline(ctx, cmdline, env)?;
    raise_nofile_limit();
    #[cfg(not(target_os = "macos"))]
    crate::install_invoker_supplementary_groups(ctx.sudo_uid(), ctx.sudo_gid())?;
    bindings::krun_start_enter(ctx.id).context("Failed to start VM")?;

    Ok(())
}

pub(crate) struct VMOutput {
    pub(crate) status: i32,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

pub(crate) fn read_all_from_fd(fd: i32) -> anyhow::Result<Vec<u8>> {
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

pub(crate) fn run_vmcommand_short(
    config: &Config,
    dev_info: &[DevInfo],
    net_mode: NetworkMode,
    opts: VMOpts,
    args: &[BString],
    process_stdin: Option<impl FnOnce(libc::c_int) -> anyhow::Result<()>>,
) -> anyhow::Result<VMOutput> {
    let forked = utils::fork_with_piped_output()?;
    if forked.pid == 0 {
        // child process
        let ctx = setup_vm(config, dev_info, net_mode, false, opts)?;
        start_vm(&ctx, &args, &[])?;
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
