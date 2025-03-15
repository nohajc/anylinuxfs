use anyhow::Context;
use std::fs;
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

    // std::thread::sleep(std::time::Duration::from_secs(30));
    Ok(())
}
