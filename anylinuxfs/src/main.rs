use anyhow::Context;
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use std::{
    env,
    ffi::CString,
    fs::{File, OpenOptions, remove_file},
    io,
    os::{fd::AsRawFd, unix::ffi::OsStrExt},
    path::{Path, PathBuf},
};
use wait_timeout::ChildExt;

#[allow(unused)]
mod bindings;

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {:#}", e);
        std::process::exit(1);
    }
}

struct Config {
    disk_path: String,
    read_only: bool,
    root_path: PathBuf,
    sudo_uid: Option<libc::uid_t>,
    sudo_gid: Option<libc::gid_t>,
}

fn load_config() -> anyhow::Result<Config> {
    let args: Vec<String> = env::args().collect();
    let disk_path = if args.len() > 1 {
        args[1].as_str()
    } else {
        eprintln!("No disk path provided");
        std::process::exit(1);
    }
    .to_owned();

    let read_only = true; // TODO: make this configurable
    let root_path = env::current_exe()
        .context("Failed to get current executable path")?
        .parent()
        .context("Failed to get executable directory")?
        .join("vmroot");

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

    Ok(Config {
        disk_path,
        read_only,
        root_path,
        sudo_uid,
        sudo_gid,
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

fn setup_and_start_vm(config: &Config, disk_fd_path: &str) -> anyhow::Result<()> {
    let ctx = unsafe { bindings::krun_create_ctx() }.context("Failed to create context")?;

    // unsafe { bindings::krun_set_log_level(3) }.context("Failed to set log level")?;

    unsafe { bindings::krun_set_vm_config(ctx, 2, 1024) }.context("Failed to set VM config")?;

    unsafe { bindings::krun_set_root(ctx, CString::from_path(&config.root_path).as_ptr()) }
        .context("Failed to set root")?;

    unsafe {
        bindings::krun_add_disk(
            ctx,
            CString::new("data").unwrap().as_ptr(),
            CString::new(disk_fd_path).unwrap().as_ptr(),
            config.read_only,
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

    let args = vec![CString::new("/vmproxy").unwrap()];
    // let args = vec![CString::new("/bin/bash").unwrap()];
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

fn gvproxy_cleanup() -> anyhow::Result<()> {
    match remove_file("/tmp/vfkit.sock-krun.sock") {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("Failed to remove vfkit socket"),
    }
    match remove_file("/tmp/vfkit.sock") {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("Failed to remove vfkit socket"),
    }
    Ok(())
}

fn start_gvproxy() -> anyhow::Result<Child> {
    gvproxy_cleanup()?;

    let gvproxy_path = "/opt/homebrew/Cellar/podman/5.4.0/libexec/podman/gvproxy";
    let gvproxy_args = [
        "--listen",
        "unix:///tmp/network.sock",
        "--listen-vfkit",
        "unixgram:///tmp/vfkit.sock",
    ];

    let gvproxy_process = Command::new(gvproxy_path)
        .args(&gvproxy_args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("Failed to start gvproxy process")?;

    Ok(gvproxy_process)
}

fn get_fd_path(disk_file: &File) -> anyhow::Result<String> {
    let disk_fd = disk_file.as_raw_fd();
    let disk_fd_path = format!("/dev/fd/{}", disk_fd);
    println!("disk_fd_path: {}", &disk_fd_path);

    // Set O_CLOEXEC on the disk file descriptor
    let flags = unsafe { libc::fcntl(disk_fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error()).context("Failed to get file descriptor flags");
    }
    if unsafe { libc::fcntl(disk_fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error())
            .context("Failed to set O_CLOEXEC on file descriptor");
    }

    Ok(disk_fd_path)
}

fn wait_for_port(port: u16) -> anyhow::Result<bool> {
    let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    for _ in 0..10 {
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
    Command::new("osascript")
        .arg("-e")
        .arg(apple_script)
        .spawn()?;
    Ok(())
}

fn run() -> anyhow::Result<()> {
    // println!("uid = {}", unsafe { libc::getuid() });
    // println!("gid = {}", unsafe { libc::getgid() });

    let config = load_config()?;

    println!("disk_path: {}", config.disk_path);
    println!("root_path: {}", config.root_path.to_string_lossy());

    let disk_file = OpenOptions::new()
        .read(true)
        .write(!config.read_only)
        .open(&config.disk_path)?;

    let disk_fd_path = get_fd_path(&disk_file)?;

    // drop privileges back to the original user if he used sudo
    drop_privileges(config.sudo_uid, config.sudo_gid)?;

    let mut gvproxy = start_gvproxy()?;
    let gvproxy_pid = gvproxy.id();

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error()).context("Failed to fork process");
    } else if pid == 0 {
        if let Some(status) = gvproxy.try_wait().ok().flatten() {
            println!(
                "gvproxy exited with status: {}",
                status
                    .code()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "unknown".to_string())
            );
            std::process::exit(1);
        }
        // Child process
        setup_and_start_vm(&config, &disk_fd_path)?;
    } else {
        // Parent process
        let is_open = wait_for_port(111).unwrap_or(false);
        println!("Port 111 is open: {}", is_open);
        // mount nfs share
        // TODO: make this configurable
        // we can even mount multiple nfs shares at once
        let share_path = "/mnt/hostblk";
        match mount_nfs(share_path) {
            Ok(_) => println!("NFS share mounted successfully"),
            Err(e) => eprintln!("Failed to mount NFS share: {:#}", e),
        }

        let mut status = 0;
        if unsafe { libc::waitpid(pid, &mut status, 0) } < 0 {
            return Err(io::Error::last_os_error()).context("Failed to wait for child process");
        }
        println!("libkrun VM exited with status: {}", status);

        // Terminate gvproxy process
        if unsafe { libc::kill(gvproxy_pid as libc::pid_t, libc::SIGTERM) } < 0 {
            return Err(io::Error::last_os_error()).context("Failed to send SIGTERM to gvproxy");
        }

        // Wait for gvproxy process to exit
        let gvproxy_status = gvproxy.wait_timeout(Duration::from_secs(5))?;
        match gvproxy_status {
            Some(status) => status.code(),
            None => {
                // Send SIGKILL to gvproxy process
                println!("timeout reached, force killing gvproxy process");
                gvproxy.kill()?;
                gvproxy.wait()?.code()
            }
        }
        .map(|s| println!("gvproxy exited with status: {}", s));
        gvproxy_cleanup()?;
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
