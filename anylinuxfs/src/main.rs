use anyhow::Context;
use std::{
    env,
    ffi::CString,
    fs::OpenOptions,
    io,
    os::{fd::AsRawFd, unix::ffi::OsStrExt},
    path::Path,
};

#[allow(unused)]
mod bindings;

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {:#}", e);
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    // println!("uid = {}", unsafe { libc::getuid() });
    // println!("gid = {}", unsafe { libc::getgid() });

    let sudo_uid = env::var("SUDO_UID")
        .map_err(anyhow::Error::from)
        .and_then(|s| Ok(s.parse::<libc::uid_t>()?))
        .ok();
    if let Some(sudo_uid) = sudo_uid {
        println!("sudo_uid = {}", sudo_uid);
    }

    let sudo_gid = env::var("SUDO_GID")
        .map_err(anyhow::Error::from)
        .and_then(|s| Ok(s.parse::<libc::gid_t>()?))
        .ok();
    if let Some(sudo_gid) = sudo_gid {
        println!("sudo_gid = {}", sudo_gid);
    }

    let args: Vec<String> = env::args().collect();
    let disk_path = if args.len() > 1 {
        args[1].as_str()
    } else {
        eprintln!("No disk path provided");
        std::process::exit(1);
    };
    let read_only = true; // TODO: make this configurable
    let root_path = env::current_exe()
        .context("Failed to get current executable path")?
        .parent()
        .context("Failed to get executable directory")?
        .join("vmroot");

    println!("disk_path: {}", disk_path);
    println!("root_path: {}", root_path.to_string_lossy());

    let disk_file = OpenOptions::new()
        .read(true)
        .write(!read_only)
        .open(disk_path)?;

    let disk_fd = format!("/dev/fd/{}", disk_file.as_raw_fd());
    println!("disk_fd: {}", &disk_fd);

    // drop privileges back to the original user if he used sudo
    // if let (Some(sudo_uid), Some(sudo_gid)) = (sudo_uid, sudo_gid) {
    //     if unsafe { libc::setgid(sudo_gid) } < 0 {
    //         return Err(io::Error::last_os_error()).context("Failed to setgid");
    //     }
    //     if unsafe { libc::setuid(sudo_uid) } < 0 {
    //         return Err(io::Error::last_os_error()).context("Failed to setuid");
    //     }
    // }

    let ctx = unsafe { bindings::krun_create_ctx() }.context("Failed to create context")?;

    // unsafe { bindings::krun_set_log_level(3) }.context("Failed to set log level")?;

    unsafe { bindings::krun_set_vm_config(ctx, 1, 512) }.context("Failed to set VM config")?;

    unsafe { bindings::krun_set_root(ctx, CString::from_path(root_path).as_ptr()) }
        .context("Failed to set root")?;

    unsafe {
        bindings::krun_add_disk(
            ctx,
            CString::new("data").unwrap().as_ptr(),
            CString::new(disk_fd).unwrap().as_ptr(),
            read_only,
        )
    }
    .context("Failed to add disk")?;

    unsafe {
        bindings::krun_set_gvproxy_path(ctx, CString::new("/tmp/vfkit.sock").unwrap().as_ptr())
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

    unsafe { bindings::krun_set_workdir(ctx, CString::new("/").unwrap().as_ptr()) }
        .context("Failed to set workdir")?;

    // let args = vec![CString::new("/vmproxy").unwrap()];
    let args = vec![CString::new("/bin/bash").unwrap()];
    let argv = args.iter().map(|s| s.as_ptr()).collect::<Vec<_>>();
    let envp = vec![std::ptr::null()];

    unsafe { bindings::krun_set_exec(ctx, argv[0], std::ptr::null(), envp.as_ptr()) }
        .context("Failed to set exec")?;

    unsafe { bindings::krun_start_enter(ctx) }.context("Failed to start VM")?;

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
