use anyhow::{Context, anyhow};
use clap::Parser;
use devinfo::DevInfo;
use nanoid::nanoid;
use objc2_core_foundation::{
    CFDictionary, CFDictionaryGetValueIfPresent, CFRetained, CFRunLoopGetCurrent, CFRunLoopRun,
    CFRunLoopStop, CFString, CFURL, CFURLGetString, kCFRunLoopDefaultMode,
};
use objc2_disk_arbitration::{
    DADisk, DADiskCopyDescription, DARegisterDiskDisappearedCallback, DASessionCreate,
    DASessionScheduleWithRunLoop, DAUnregisterCallback,
};
use std::ffi::c_void;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::ops::Deref;
use std::os::fd::FromRawFd;
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
use url::Url;
use wait_timeout::ChildExt;

#[allow(unused)]
mod bindings;
mod devinfo;
mod log;

#[allow(unused)]
mod procutils;

fn main() {
    if let Err(e) = run() {
        host_eprintln!("Error: {:#}", e);
        std::process::exit(1);
    }
}

struct Config {
    disk_path: String,
    read_only: bool,
    root_path: PathBuf,
    fetch_rootfs_path: PathBuf,
    kernel_path: PathBuf,
    gvproxy_path: PathBuf,
    vsock_path: String,
    vfkit_sock_path: String,
    sudo_uid: Option<libc::uid_t>,
    sudo_gid: Option<libc::gid_t>,
    mount_options: Option<String>,
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
struct Cli {
    disk_path: String,
    #[arg(short, long)]
    options: Option<String>,
}

fn is_read_only_set(mount_options: Option<&str>) -> bool {
    if let Some(options) = mount_options {
        options.split(',').any(|opt| opt == "ro")
    } else {
        false
    }
}

fn load_config() -> anyhow::Result<Config> {
    let cli = Cli::parse();
    let (disk_path, mount_options) = if !cli.disk_path.is_empty() {
        (cli.disk_path, cli.options)
    } else {
        host_eprintln!("No disk path provided");
        std::process::exit(1);
    };

    let read_only = is_read_only_set(mount_options.as_deref());

    let exec_dir = env::current_exe()
        .context("Failed to get current executable path")?
        .parent()
        .context("Failed to get executable directory")?
        .to_owned();

    let prefix_dir = exec_dir
        .parent()
        .context("Failed to get prefix directory")?;

    // ~/.anylinuxfs/alpine/rootfs
    let root_path = homedir::my_home()?
        .context("Failed to get home directory")?
        .join(".anylinuxfs")
        .join("alpine")
        .join("rootfs");

    let fetch_rootfs_path = exec_dir.join("fetch-rootfs").to_owned();

    let kernel_path = prefix_dir.join("libexec").join("Image").to_owned();
    let gvproxy_path = prefix_dir.join("libexec").join("gvproxy").to_owned();

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

    let vsock_path = format!("/tmp/anylinuxfs-{}-vsock", rand_string(8));
    let vfkit_sock_path = format!("/tmp/vfkit-{}.sock", rand_string(8));

    Ok(Config {
        disk_path,
        read_only,
        root_path,
        fetch_rootfs_path,
        kernel_path,
        gvproxy_path,
        vsock_path,
        vfkit_sock_path,
        sudo_uid,
        sudo_gid,
        mount_options,
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

fn setup_and_start_vm(config: &Config, dev_info: &DevInfo) -> anyhow::Result<()> {
    let ctx = unsafe { bindings::krun_create_ctx() }.context("Failed to create context")?;

    // TODO: configurable log level?
    unsafe { bindings::krun_set_log_level(3) }.context("Failed to set log level")?;

    // TODO: CPU and RAM should be configurable
    unsafe { bindings::krun_set_vm_config(ctx, 2, 1024) }.context("Failed to set VM config")?;

    // run vmm as the original user if he used sudo
    if let Some(uid) = config.sudo_uid {
        unsafe { bindings::krun_setuid(ctx, uid) }.context("Failed to set vmm uid")?;
    }

    if let Some(gid) = config.sudo_gid {
        unsafe { bindings::krun_setgid(ctx, gid) }.context("Failed to set vmm gid")?;
    }

    unsafe { bindings::krun_set_root(ctx, CString::from_path(&config.root_path).as_ptr()) }
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
            CString::new(config.vfkit_sock_path.as_str())
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

    vsock_cleanup(&config)?;

    unsafe {
        bindings::krun_add_vsock_port2(
            ctx,
            12700,
            CString::new(config.vsock_path.as_str()).unwrap().as_ptr(),
            true,
        )
    }
    .context("Failed to add vsock port")?;

    unsafe { bindings::krun_set_workdir(ctx, CString::new("/").unwrap().as_ptr()) }
        .context("Failed to set workdir")?;

    let args: Vec<_> = [
        // CString::new("/bin/bash").unwrap(),
        CString::new("/vmproxy").unwrap(),
        CString::new(dev_info.auto_mount_name()).unwrap(),
        CString::new(dev_info.fs_type().unwrap_or("auto")).unwrap(),
    ]
    .into_iter()
    .chain(
        config
            .mount_options
            .as_deref()
            .into_iter()
            .map(|s| CString::new(s).unwrap()),
    )
    .collect();

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
            CString::from_path(&config.kernel_path).as_ptr(),
            0, // KRUN_KERNEL_FORMAT_RAW
            null(),
            null(),
        )
    }
    .context("Failed to set kernel")?;

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

fn wait_for_port(port: u16) -> anyhow::Result<bool> {
    let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    for _ in 0..5 {
        let result = TcpStream::connect_timeout(&addr.into(), Duration::from_secs(10)).is_ok();
        if result {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_secs(1));
    }

    Ok(false)
}

fn mount_nfs(share_path: &str) -> anyhow::Result<()> {
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
    let _ = stream.read(&mut buf)?;

    Ok(())
}

fn terminate_child(child: &mut Child, child_name: &str) -> anyhow::Result<()> {
    // Terminate child process
    if unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) } < 0 {
        return Err(io::Error::last_os_error())
            .context(format!("Failed to send SIGTERM to {child_name}"));
    }

    // Wait for child process to exit
    let child_status = child.wait_timeout(Duration::from_secs(5))?;
    match child_status {
        Some(status) => status.code(),
        None => {
            // Send SIGKILL to child process
            host_println!("timeout reached, force killing {child_name} process");
            child.kill()?;
            child.wait()?.code()
        }
    }
    .map(|s| host_println!("{} exited with status: {}", child_name, s));

    Ok(())
}

fn wait_for_file(file: impl AsRef<Path>) -> anyhow::Result<()> {
    let start = std::time::Instant::now();
    while !file.as_ref().exists() {
        if start.elapsed() > Duration::from_secs(10) {
            return Err(anyhow!(
                "Timeout waiting for file creation: {}",
                file.as_ref().to_string_lossy()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn init_rootfs_if_needed(config: &Config) -> anyhow::Result<()> {
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
        host_println!("VM root filesystem is initialized");
        return Ok(());
    }

    host_println!("Initializing VM root filesystem...");

    let mut fetch_rootfs_cmd = Command::new(&config.fetch_rootfs_path);
    if let (Some(uid), Some(gid)) = (config.sudo_uid, config.sudo_gid) {
        // run fetch-rootfs with dropped privileges
        fetch_rootfs_cmd.uid(uid).gid(gid);
    }

    let status = fetch_rootfs_cmd
        .status()
        .context("Failed to execute fetch-rootfs")?;

    if !status.success() {
        return Err(anyhow!(
            "fetch-rootfs failed with exit code {}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or("unknown".to_owned())
        ));
    }

    Ok(())
}

fn run() -> anyhow::Result<()> {
    // host_println!("uid = {}", unsafe { libc::getuid() });
    // host_println!("gid = {}", unsafe { libc::getgid() });

    let config = load_config()?;

    init_rootfs_if_needed(&config)?;

    // host_println!("disk_path: {}", config.disk_path);
    host_println!("root_path: {}", config.root_path.to_string_lossy());

    let dev_info = DevInfo::new(&config.disk_path)?;

    host_println!("disk: {}", dev_info.disk());
    host_println!("rdisk: {}", dev_info.rdisk());
    host_println!("label: {:?}", dev_info.label());
    host_println!("fs_type: {:?}", dev_info.fs_type());
    host_println!("uuid: {:?}", dev_info.uuid());
    host_println!("mount name: {}", dev_info.auto_mount_name());

    let mut gvproxy = start_gvproxy(&config)?;

    let forked = procutils::fork_with_pty_output()?;
    if forked.pid == 0 {
        // Child process
        // procutils::set_null_stdin()?;

        wait_for_file(&config.vfkit_sock_path)?;

        if let Some(status) = gvproxy.try_wait().ok().flatten() {
            host_println!(
                "gvproxy failed with exit code: {}",
                status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or("unknown".to_owned())
            );
            std::process::exit(1);
        }

        setup_and_start_vm(&config, &dev_info)?;
    } else {
        // Parent process

        // Spawn a thread to read from the pipe
        let hnd = thread::spawn(move || {
            let mut buf_reader = BufReader::new(unsafe { File::from_raw_fd(forked.child_read_fd) });
            let mut line = String::new();
            while let Ok(bytes) = buf_reader.read_line(&mut line) {
                if bytes == 0 {
                    break; // EOF
                }
                print!("Linux: {}", line);
                line.clear();
            }
        });

        // drop privileges back to the original user if he used sudo
        drop_privileges(config.sudo_uid, config.sudo_gid)?;

        let is_open = wait_for_port(111).unwrap_or(false);

        if is_open {
            host_println!("Port 111 is open");
            // mount nfs share
            let share_name = dev_info.auto_mount_name();
            let share_path = format!("/mnt/{share_name}");
            match mount_nfs(&share_path) {
                Ok(_) => host_println!("NFS share mounted successfully"),
                Err(e) => host_eprintln!("Failed to mount NFS share: {:#}", e),
            }

            wait_for_unmount(share_name)?;
            send_quit_cmd(&config)?;
        } else {
            host_println!("Port 111 is not open");
        }

        vsock_cleanup(&config)?;

        let mut status = 0;
        if unsafe { libc::waitpid(forked.pid, &mut status, 0) } < 0 {
            return Err(io::Error::last_os_error()).context("Failed to wait for child process");
        }
        host_println!("libkrun VM exited with status: {}", status);

        // Terminate gvproxy process
        terminate_child(&mut gvproxy, "gvproxy")?;
        gvproxy_cleanup(&config)?;

        hnd.join().unwrap();
    }

    Ok(())
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
