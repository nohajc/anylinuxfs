use anyhow::{Context, anyhow};
use clap::Parser;
use libc::VMADDR_CID_ANY;
use serde::Serialize;
use std::io::{BufRead, Write};
use std::process::{Child, Command, ExitCode};
use std::time::Duration;
use std::{fs, io::BufReader};
use sys_mount::{UnmountFlags, unmount};
use vsock::{VsockAddr, VsockListener};

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    mount_name: Option<String>,
    #[arg(short = 't', long = "types")]
    fs_type: Option<String>,
    #[arg(short = 'o', long = "options")]
    mount_options: Option<String>,
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Serialize, Debug)]
struct PortDef<'a> {
    local: &'a str,
    remote: &'a str,
}

const EXPOSE_PORT_SVC: &str = "http://192.168.127.1/services/forwarder/expose";

fn expose_port(client: &reqwest::blocking::Client, port_def: &PortDef) -> anyhow::Result<()> {
    client
        .post(EXPOSE_PORT_SVC)
        .json(port_def)
        .send()
        .and_then(|res| res.error_for_status())
        .context(format!("Failed to expose port: {:?}", port_def))?;

    Ok(())
}

fn init_network() -> anyhow::Result<()> {
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

    let client = reqwest::blocking::Client::new();
    expose_port(
        &client,
        &PortDef {
            local: ":111",
            remote: "192.168.127.2:111",
        },
    )?;
    expose_port(
        &client,
        &PortDef {
            local: "127.0.0.1:2049",
            remote: "192.168.127.2:2049",
        },
    )?;
    expose_port(
        &client,
        &PortDef {
            local: "127.0.0.1:32765",
            remote: "192.168.127.2:32765",
        },
    )?;
    expose_port(
        &client,
        &PortDef {
            local: "127.0.0.1:32767",
            remote: "192.168.127.2:32767",
        },
    )?;

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

fn is_read_only_set(mount_options: Option<&str>) -> bool {
    if let Some(options) = mount_options {
        options.split(',').any(|opt| opt == "ro")
    } else {
        false
    }
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
    // println!("uid = {}", unsafe { libc::getuid() });
    // println!("gid = {}", unsafe { libc::getgid() });

    // let kernel_cfg = procfs::kernel_config()?;
    // println!("Kernel config");
    // for (key, value) in kernel_cfg {
    //     println!("{} = {:?}", key, value);
    // }

    let cli = Cli::parse();

    let mount_point = format!("/mnt/{}", cli.mount_name.unwrap_or("hostblk".to_owned()));
    let fs_type = cli.fs_type;
    let mount_options = cli.mount_options;
    let verbose = cli.verbose;

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

    init_network().context("Failed to initialize network")?;

    // list_dir(mount_point);

    let mode = if is_read_only_set(mount_options.as_deref()) {
        "ro"
    } else {
        "rw"
    };

    let exports_content = format!(
        "{}      *({},no_subtree_check,no_root_squash,insecure)\n",
        &mount_point, mode,
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
    Ok(())
}
