use anyhow::Context;
use libc::VMADDR_CID_ANY;
use std::io::{self, BufRead, Write};
use std::process::Command;
use std::time::Duration;
use std::{fs, io::BufReader};
use sys_mount::{FilesystemType, Mount, MountFlags, SupportedFilesystems, Unmount, UnmountFlags};
use vsock::{VsockAddr, VsockListener};

fn _list_dir(dir: &str) {
    match fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries {
                if let Ok(entry) = entry {
                    println!("{}", entry.path().display());
                }
            }
        }
        Err(e) => eprintln!("Failed to read {} directory: {}", dir, e),
    }
}

fn init_network() -> anyhow::Result<()> {
    // TODO: execute the script commands directly
    let mut hnd = Command::new("/bin/sh")
        .arg("/init-network.sh")
        .spawn()
        .context("Failed to execute /init-network.sh")?;

    hnd.wait()
        .context("Failed to wait for /init-network.sh to finish")?;
    Ok(())
}

fn wait_for_quit_cmd() -> anyhow::Result<()> {
    let addr = VsockAddr::new(VMADDR_CID_ANY, 12700);
    let listener = VsockListener::bind(&addr)?;

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

fn main() -> anyhow::Result<()> {
    println!("Hello, world, from linux microVM!");
    println!("uid = {}", unsafe { libc::getuid() });
    println!("gid = {}", unsafe { libc::getgid() });
    println!("");

    // let kernel_cfg = procfs::kernel_config()?;
    // println!("Kernel config");
    // for (key, value) in kernel_cfg {
    //     println!("{} = {:?}", key, value);
    // }

    init_network().context("Failed to initialize network")?;

    // TODO: take from command line
    let mount_point = "/mnt/hostblk";

    fs::create_dir_all(mount_point)
        .context(format!("Failed to create directory '{mount_point}'"))?;
    println!("Directory '{mount_point}' created successfully.");

    let supported_fs =
        SupportedFilesystems::new().context("Failed to get supported filesystems")?;

    // for fs in supported_fs.dev_file_systems() {
    //     println!("Supported filesystem: {:?}", fs);
    // }

    // for fs in supported_fs.nodev_file_systems() {
    //     println!("Supported nodev filesystem: {:?}", fs);
    // }

    let mounted = Mount::builder()
        .fstype(FilesystemType::from(&supported_fs))
        .flags(MountFlags::RDONLY)
        .mount("/dev/vda", mount_point)
        .context(format!("Failed to mount '/dev/vda' on '{mount_point}'"))?;

    println!(
        "'/dev/vda' mounted successfully on '{mount_point}', recognized as {}.",
        mounted.get_fstype()
    );

    // list_dir(mount_point);

    let mut hnd = Command::new("/usr/local/bin/entrypoint.sh")
        // .env("NFS_VERSION", "3")
        // .env("NFS_DISABLE_VERSION_3", "1")
        .spawn()
        .context("Failed to execute /usr/local/bin/entrypoint.sh")?;

    wait_for_quit_cmd()?;

    // TODO: we should also wait with timeout and SIGKILL if necessary
    if unsafe { libc::kill(hnd.id() as i32, libc::SIGTERM) } < 0 {
        return Err(io::Error::last_os_error()).context(format!("Failed to send SIGTERM"));
    }
    hnd.wait()
        .context("Failed to wait for /usr/local/bin/entrypoint.sh to finish")?;

    let mut backoff = Duration::from_secs(1);
    while let Err(e) = mounted.unmount(UnmountFlags::empty()) {
        eprintln!("Failed to unmount '{mount_point}': {}", e);
        std::thread::sleep(backoff);
        backoff = std::cmp::min(backoff * 2, Duration::from_secs(32));
    }
    println!("Unmounted '{mount_point}' successfully.");
    Ok(())
}
