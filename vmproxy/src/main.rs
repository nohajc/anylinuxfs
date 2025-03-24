use anyhow::{Context, anyhow};
use libc::VMADDR_CID_ANY;
use std::env;
use std::io::{self, BufRead, Write};
use std::process::Command;
use std::time::Duration;
use std::{fs, io::BufReader};
use sys_mount::{UnmountFlags, unmount};
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
    // println!("uid = {}", unsafe { libc::getuid() });
    // println!("gid = {}", unsafe { libc::getgid() });
    println!("");

    // let kernel_cfg = procfs::kernel_config()?;
    // println!("Kernel config");
    // for (key, value) in kernel_cfg {
    //     println!("{} = {:?}", key, value);
    // }

    init_network().context("Failed to initialize network")?;

    let mount_point = format!(
        "/mnt/{}",
        env::args().nth(1).unwrap_or("hostblk".to_owned())
    );

    let fs_type = env::args().nth(2);
    let mount_options = env::args().nth(3);

    fs::create_dir_all(&mount_point)
        .context(format!("Failed to create directory '{}'", &mount_point))?;
    println!("Directory '{}' created successfully.", &mount_point);

    // let supported_fs =
    //     SupportedFilesystems::new().context("Failed to get supported filesystems")?;

    // for fs in supported_fs.dev_file_systems() {
    //     println!("Supported filesystem: {:?}", fs);
    // }

    // for fs in supported_fs.nodev_file_systems() {
    //     println!("Supported nodev filesystem: {:?}", fs);
    // }

    // let mounted = Mount::builder()
    //     .fstype(FilesystemType::from(&supported_fs))
    //     .flags(MountFlags::RDONLY)
    //     // .data(data)
    //     .mount("/dev/vda", &mount_point)
    //     .context(format!("Failed to mount '/dev/vda' on '{}'", &mount_point))?;

    let mnt_args = [
        "-t",
        fs_type.as_deref().unwrap_or("auto"),
        "/dev/vda",
        &mount_point,
    ]
    .into_iter()
    .chain(
        mount_options
            .as_deref()
            .into_iter()
            .flat_map(|opts| ["-o", opts]),
    );

    let mnt_args: Vec<&str> = mnt_args.collect();
    println!("mount args: {:?}", &mnt_args);

    let mut mnt_cmd = Command::new("/bin/mount")
        .args(mnt_args)
        .spawn()
        .context("Failed to run mount command")?;

    let mnt_result = mnt_cmd.wait().context("Failed to wait for mount command")?;
    if !mnt_result.success() {
        return Err(anyhow!(
            "Mounting {} on {} failed with error code {}",
            "/dev/vda",
            &mount_point,
            mnt_result
                .code()
                .map(|c| c.to_string())
                .unwrap_or("unknown".to_owned())
        ));
    }

    println!(
        "'/dev/vda' mounted successfully on '{}', filesystem {}.",
        &mount_point,
        fs_type.unwrap_or("unknown".to_owned())
    );

    // list_dir(mount_point);

    let exports_content = format!(
        "{}      *(ro,no_subtree_check,no_root_squash,insecure)\n",
        &mount_point
    );

    fs::write("/etc/exports", exports_content).context("Failed to write to /etc/exports")?;
    println!("Successfully initialized /etc/exports.");

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
    while let Err(e) = unmount(&mount_point, UnmountFlags::empty()) {
        eprintln!("Failed to unmount '{}': {}", &mount_point, e);
        std::thread::sleep(backoff);
        backoff = std::cmp::min(backoff * 2, Duration::from_secs(32));
    }
    println!("Unmounted '{mount_point}' successfully.");
    Ok(())
}
