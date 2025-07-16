use anyhow::{Context, anyhow};
use clap::Parser;
use libc::VMADDR_CID_ANY;
use serde::Serialize;
use std::io::{BufRead, Write};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::Duration;
use std::{fs, io::BufReader};
use sys_mount::{UnmountFlags, unmount};
use vsock::{VsockAddr, VsockListener};

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    disk_path: String,
    mount_name: String,
    #[arg(short = 't', long = "types")]
    fs_type: Option<String>,
    #[arg(long = "fs-driver")]
    fs_driver: Option<String>,
    #[arg(short = 'o', long = "options")]
    mount_options: Option<String>,
    #[arg(short, long)]
    decrypt: Option<String>,
    #[arg(short, long, default_value = LOCALHOST)]
    bind_addr: String,
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Serialize, Debug)]
struct PortDef<'a> {
    local: &'a str,
    remote: &'a str,
}

const EXPOSE_PORT_SVC: &str = "http://192.168.127.1/services/forwarder/expose";
const LOCALHOST: &str = "127.0.0.1";

fn expose_port(client: &reqwest::blocking::Client, port_def: &PortDef) -> anyhow::Result<()> {
    client
        .post(EXPOSE_PORT_SVC)
        .json(port_def)
        .send()
        .and_then(|res| res.error_for_status())
        .context(format!("Failed to expose port: {:?}", port_def))?;

    Ok(())
}

fn init_network(bind_addr: &str) -> anyhow::Result<()> {
    fs::write("/etc/resolv.conf", "nameserver 192.168.127.1\n")
        .context("Failed to write /etc/resolv.conf")?;

    Command::new("/bin/sh")
        .arg("-c")
        .arg(
            "ip addr add 192.168.127.2/24 dev eth0 \
            && ip link set eth0 up \
            && ip route add default via 192.168.127.1 dev eth0",
        )
        .status()
        .context("Failed to configure network interface")?;

    let mut bind_addr_list = vec![bind_addr];
    if bind_addr != LOCALHOST {
        bind_addr_list.push(LOCALHOST);
    }

    let client = reqwest::blocking::Client::new();
    expose_port(
        &client,
        &PortDef {
            local: ":111",
            remote: "192.168.127.2:111",
        },
    )?;

    for addr in bind_addr_list {
        expose_port(
            &client,
            &PortDef {
                local: &format!("{addr}:2049"),
                remote: "192.168.127.2:2049",
            },
        )?;
        expose_port(
            &client,
            &PortDef {
                local: &format!("{addr}:32765"),
                remote: "192.168.127.2:32765",
            },
        )?;
        expose_port(
            &client,
            &PortDef {
                local: &format!("{addr}:32767"),
                remote: "192.168.127.2:32767",
            },
        )?;
    }

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

fn is_read_only_set<'a>(mut mount_options: impl Iterator<Item = &'a str>) -> bool {
    mount_options.any(|opt| opt == "ro")
}

fn terminate_child(child: &mut Child, child_name: &str) -> anyhow::Result<()> {
    common_utils::terminate_child(child, child_name, None)
}

fn main() -> ExitCode {
    if let Err(e) = run() {
        eprintln!("Error: {:#}", e);
        eprintln!("<anylinuxfs-exit-code:1>");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn run() -> anyhow::Result<()> {
    // println!("vmproxy started");
    // println!("uid = {}", unsafe { libc::getuid() });
    // println!("gid = {}", unsafe { libc::getgid() });

    // let kernel_cfg = procfs::kernel_config()?;
    // println!("Kernel config");
    // for (key, value) in kernel_cfg {
    //     println!("{} = {:?}", key, value);
    // }

    let cli = Cli::parse();

    let mut disk_path = cli.disk_path;
    let mut fs_type = cli.fs_type;
    let fs_driver = cli.fs_driver;
    let mount_options = cli.mount_options;
    let verbose = cli.verbose;

    let (mapper_ident_prefix, cryptsetup_op) = match fs_type.as_deref() {
        Some("crypto_LUKS") => ("luks", "open"),
        Some("BitLocker") => ("btlk", "bitlkOpen"),
        _ => ("luks", "open"),
    };

    // decrypt LUKS/BitLocker volumes if any
    if let Some(decrypt) = &cli.decrypt {
        for (i, dev) in decrypt.split(",").enumerate() {
            let cryptsetup_result = Command::new("/sbin/cryptsetup")
                .arg(cryptsetup_op)
                .arg(&dev)
                .arg(format!("{mapper_ident_prefix}{i}"))
                .stdout(Stdio::null())
                .status()
                .context(format!("Failed to run cryptsetup command for '{}'", dev))?;

            if !cryptsetup_result.success() {
                return Err(anyhow!(
                    "Failed to open encrypted device '{}': {}",
                    dev,
                    cryptsetup_result
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or("unknown".to_owned())
                ));
            }
        }
    }

    // activate RAID volumes if any
    let is_raid = disk_path.starts_with("/dev/md");
    if is_raid {
        let _mdadm_assemble_result = Command::new("/sbin/mdadm")
            .arg("--assemble")
            .arg("--scan")
            .status()
            .context("Failed to run mdadm command")?;

        let output = Command::new("/bin/busybox")
            .arg("sh")
            .arg("-c")
            .arg("mdadm --detail --scan | cut -d' ' -f2")
            .output()
            .context("Failed to get RAID device path from mdadm")?;

        let md_path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !md_path.is_empty() {
            disk_path = md_path;
        }
    }

    // activate LVM volumes if any
    // vgchange can return non-zero but still partially succeed
    let _vgchange_result = Command::new("/sbin/vgchange")
        .arg("-ay")
        .status()
        .context("Failed to run vgchange command")?;

    match fs_type.as_deref() {
        Some("crypto_LUKS") => {
            disk_path = "/dev/mapper/luks0".into();
            fs_type = None;
        }
        Some("BitLocker") => {
            disk_path = "/dev/mapper/btlk0".into();
            fs_type = None;
        }
        _ => {}
    }
    let is_logical = disk_path.starts_with("/dev/mapper") || is_raid;

    let name = &cli.mount_name;
    let mount_name = if !is_logical {
        name.to_owned()
    } else {
        let label = Command::new("/sbin/blkid")
            .arg(&disk_path)
            .arg("-s")
            .arg("LABEL")
            .arg("-o")
            .arg("value")
            .output()
            .context("Failed to run blkid command")?
            .stdout;

        let mut label = String::from_utf8_lossy(&label).trim().to_owned();
        if label.is_empty() {
            label = name.to_owned();
        }
        println!("<anylinuxfs-label:{}>", &label);
        label
    };

    match fs_type.as_deref() {
        Some("auto") | None => {
            let fs = Command::new("/sbin/blkid")
                .arg(&disk_path)
                .arg("-s")
                .arg("TYPE")
                .arg("-o")
                .arg("value")
                .output()
                .context("Failed to run blkid command")?
                .stdout;

            let fs = String::from_utf8_lossy(&fs).trim().to_owned();
            println!("<anylinuxfs-type:{}>", &fs);
            fs_type = if !fs.is_empty() { Some(fs) } else { None };
        }
        _ => (),
    }

    let mount_point = format!("/mnt/{}", mount_name);

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
        fs_driver
            .as_deref()
            .or(fs_type.as_deref())
            .unwrap_or("auto"),
        &disk_path,
        &mount_point,
    ]
    .into_iter()
    .chain(
        mount_options
            .as_deref()
            .into_iter()
            .flat_map(|opts| ["-o", opts]),
    )
    .chain(verbose.then_some("-v").into_iter());

    let mnt_args: Vec<&str> = mnt_args.collect();
    println!("mount args: {:?}", &mnt_args);

    let mnt_result = Command::new("/bin/mount")
        .args(mnt_args)
        .status()
        .context("Failed to run mount command")?;

    if !mnt_result.success() {
        return Err(anyhow!(
            "Mounting {} on {} failed with error code {}",
            &disk_path,
            &mount_point,
            mnt_result
                .code()
                .map(|c| c.to_string())
                .unwrap_or("unknown".to_owned())
        ));
    }

    println!(
        "'{}' mounted successfully on '{}', filesystem {}.",
        &disk_path,
        &mount_point,
        fs_type.unwrap_or("unknown".to_owned())
    );

    let effective_mount_options = {
        let output = Command::new("/bin/busybox")
            .arg("sh")
            .arg("-c")
            .arg(format!(
                "mount | grep {} | awk -F'(' '{{ print $2 }}' | tr -d ')'",
                &disk_path
            ))
            .output()
            .context(format!("Failed to get mount options for {}", &disk_path))?;

        let opts = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        println!("Effective mount options: {}", opts);
        opts
    }
    .split(',')
    .map(|s| s.to_owned())
    .collect::<Vec<String>>();

    init_network(&cli.bind_addr).context("Failed to initialize network")?;

    // list_dir(mount_point);

    let specified_read_only = mount_options
        .as_deref()
        .map(|opts| is_read_only_set(opts.split(',')))
        .unwrap_or(false);
    let effective_read_only = is_read_only_set(effective_mount_options.iter().map(String::as_str));

    if specified_read_only != effective_read_only {
        println!("<anylinuxfs-mount:changed-to-ro>");
    }

    let export_mode = if effective_read_only { "ro" } else { "rw" };
    let exports_content = format!(
        "\"{}\"      *({},no_subtree_check,no_root_squash,insecure)\n",
        &mount_point, export_mode,
    );

    fs::write("/etc/exports", exports_content).context("Failed to write to /etc/exports")?;
    println!("Successfully initialized /etc/exports.");

    // let curl_result = Command::new("curl")
    //     .arg("ifconfig.co")
    //     .status()
    //     .context("Failed to execute curl to check internet connectivity")?;

    // if !curl_result.success() {
    //     return Err(anyhow!(
    //         "Curl command failed with error code {}",
    //         curl_result
    //             .code()
    //             .map(|c| c.to_string())
    //             .unwrap_or("unknown".to_owned())
    //     ));
    // }

    match Command::new("/usr/local/bin/entrypoint.sh")
        // .env("NFS_VERSION", "3")
        // .env("NFS_DISABLE_VERSION_3", "1")
        .spawn()
    {
        Ok(mut hnd) => {
            if let Err(e) = wait_for_quit_cmd() {
                eprintln!("Error while waiting for quit command: {:#}", e);
            }

            if let Err(e) = terminate_child(&mut hnd, "entrypoint.sh") {
                eprintln!("{:#}", e);
            }
        }
        Err(e) => {
            eprintln!("Failed to start entrypoint.sh: {:#}", e);
        }
    }

    let mut backoff = Duration::from_secs(1);
    while let Err(e) = unmount(&mount_point, UnmountFlags::empty()) {
        eprintln!("Failed to unmount '{}': {}", &mount_point, e);
        std::thread::sleep(backoff);
        backoff = std::cmp::min(backoff * 2, Duration::from_secs(32));
    }
    println!("Unmounted '{mount_point}' successfully.");

    _ = fs::remove_dir_all(&mount_point);
    Ok(())
}
