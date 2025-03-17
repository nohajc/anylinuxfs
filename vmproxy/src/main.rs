use anyhow::Context;
use std::fs;
use std::process::Command;
use sys_mount::{FilesystemType, Mount, MountFlags, SupportedFilesystems};

fn list_dir(dir: &str) {
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

    fs::create_dir_all("/mnt/hostblk").context("Failed to create directory '/mnt/hostblk'")?;
    println!("Directory '/mnt/hostblk' created successfully.");

    let supported_fs =
        SupportedFilesystems::new().context("Failed to get supported filesystems")?;

    // for fs in supported_fs.dev_file_systems() {
    //     println!("Supported filesystem: {:?}", fs);
    // }

    // for fs in supported_fs.nodev_file_systems() {
    //     println!("Supported nodev filesystem: {:?}", fs);
    // }

    let result = Mount::builder()
        .fstype(FilesystemType::from(&supported_fs))
        .flags(MountFlags::RDONLY)
        .mount("/dev/vda", "/mnt/hostblk")
        .context("Failed to mount '/dev/vda' on '/mnt/hostblk'")?;

    println!(
        "'/dev/vda' mounted successfully on '/mnt/hostblk', recognized as {}.",
        result.get_fstype()
    );

    list_dir("/mnt/hostblk");

    let mut hnd = Command::new("/usr/local/bin/entrypoint.sh")
        .env("NFS_VERSION", "3")
        .spawn()
        .context("Failed to execute /usr/local/bin/entrypoint.sh")?;

    hnd.wait()
        .context("Failed to wait for /usr/local/bin/entrypoint.sh to finish")?;
    Ok(())
}
