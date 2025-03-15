use rustix::mount::{MountFlags, mount};
use std::fs;

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

fn main() {
    println!("Hello, world, from linux microVM!");

    match fs::create_dir_all("/mnt/hostblk") {
        Ok(_) => println!("Directory '/mnt/hostblk' created successfully."),
        Err(e) => eprintln!("Failed to create directory '/mnt/hostblk': {}", e),
    }

    match mount(
        "/dev/vda",
        "/mnt/hostblk",
        "btrfs",
        MountFlags::RDONLY,
        None,
    ) {
        Ok(_) => println!("'/dev/vda' mounted successfully on '/mnt/hostblk'."),
        Err(e) => eprintln!("Failed to mount '/dev/vda' on '/mnt/hostblk': {}", e),
    }

    list_dir("/mnt/hostblk/home/nohajc");

    // std::thread::sleep(std::time::Duration::from_secs(30));
}
