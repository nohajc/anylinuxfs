use std::{
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::{fs::chown, net::UnixStream, process::CommandExt},
    process::{Child, Command},
    time::Duration,
};

use std::{io::BufReader, process::Stdio};

use crate::{
    netutil,
    settings::{Config, Preferences},
};
use anyhow::{Context, anyhow};
use common_utils::{OSType, VM_CTRL_PORT, VM_IP, VMNET_PREFIX_LEN, host_println};
use ipnet::Ipv4Net;
use rand::prelude::*;
use serde::Deserialize;
use serde_json::Deserializer;

pub fn random_mac_address() -> [u8; 6] {
    let mut rng = rand::rng();
    return [
        0x00,
        0x16,
        0x3e,
        rng.random_range(0x00..=0x7f),
        rng.random_range(0x00..=0xff),
        rng.random_range(0x00..=0xff),
    ];
}

pub fn vfkit_sock_cleanup(vfkit_sock_path: &str) -> anyhow::Result<()> {
    let sock_krun_path = vfkit_sock_path.replace(".sock", ".sock-krun.sock");
    match fs::remove_file(&sock_krun_path) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("Failed to remove vfkit socket"),
    }
    match fs::remove_file(&vfkit_sock_path) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("Failed to remove vfkit socket"),
    }
    Ok(())
}

pub fn vsock_cleanup(vsock_path: &str) -> anyhow::Result<()> {
    match fs::remove_file(vsock_path) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("Failed to remove vsock socket"),
    }
    Ok(())
}

#[allow(unused)]
#[derive(Debug, Deserialize)]
pub struct VmnetConfigJson {
    pub vmnet_write_max_packets: u32,
    pub vmnet_read_max_packets: u32,
    pub vmnet_subnet_mask: String,
    pub vmnet_mtu: u32,
    pub vmnet_end_address: String,
    pub vmnet_start_address: String,
    pub vmnet_interface_id: String,
    pub vmnet_max_packet_size: u32,
    pub vmnet_nat66_prefix: String,
    pub vmnet_mac_address: String,
}

pub struct VmnetConfig {
    pub _helper_output: VmnetConfigJson,
    pub vmnet_cidr: Ipv4Net,
}

pub fn start_vmnet_helper(config: &Config) -> anyhow::Result<(Child, VmnetConfig)> {
    vfkit_sock_cleanup(&config.unixgram_sock_path)?;

    let known_networks =
        netutil::get_interface_networks().context("Failed to get interface networks")?;

    let vmnet_cidr = netutil::pick_available_network(VMNET_PREFIX_LEN, &known_networks)
        .context("Failed to find available network for vmnet-helper")?;

    let mut vmnet_helper_cmd = Command::new(&config.vmnet_helper_path);

    let vmnet_helper_err = File::create(&config.nethelper_log_path)
        .context("Failed to create vmnet-helper.log file")?;

    chown(
        &config.nethelper_log_path,
        Some(config.invoker_uid),
        Some(config.invoker_gid),
    )
    .with_context(|| {
        format!(
            "Failed to change owner of {}",
            config.nethelper_log_path.display()
        )
    })?;

    vmnet_helper_cmd
        .arg("--socket")
        .arg(&config.unixgram_sock_path)
        .args([
            // "--enable-tso",
            // "--enable-checksum-offload",
            &format!("--start-address={}", vmnet_cidr.hosts().next().unwrap()),
            &format!("--end-address={}", vmnet_cidr.hosts().last().unwrap()),
            &format!("--subnet-mask={}", vmnet_cidr.netmask()),
            "--operation-mode=shared",
        ])
        .stdout(Stdio::piped())
        .stderr(vmnet_helper_err);

    if let (Some(uid), Some(gid)) = (config.sudo_uid, config.sudo_gid) {
        // run vmnet-helper with dropped privileges
        vmnet_helper_cmd.uid(uid).gid(gid);
    }

    let mut vmnet_helper_process = vmnet_helper_cmd
        .spawn()
        .context("Failed to start vmnet-helper process")?;

    let child_out = BufReader::new(vmnet_helper_process.stdout.take().unwrap());
    // host_println!("Waiting for vmnet-helper to output config...");
    let mut config_de = Deserializer::from_reader(child_out);
    let _helper_output = VmnetConfigJson::deserialize(&mut config_de)
        .context("Failed to parse vmnet-helper config")?;

    let vmnet_config = VmnetConfig {
        _helper_output,
        vmnet_cidr,
    };

    Ok((vmnet_helper_process, vmnet_config))
}

pub fn start_gvproxy(config: &Config) -> anyhow::Result<Child> {
    vfkit_sock_cleanup(&config.unixgram_sock_path)?;

    let net_sock_uri = format!("unix://{}", &config.gvproxy_net_sock_path);
    let vfkit_sock_uri = format!("unixgram://{}", &config.unixgram_sock_path);
    let mut gvproxy_args = vec![
        "--listen",
        &net_sock_uri,
        "--listen-vfkit",
        &vfkit_sock_uri,
        "--ssh-port",
        "-1",
    ];

    if config.preferences.gvproxy_debug() {
        gvproxy_args.push("--debug");
    }

    let mut gvproxy_cmd = Command::new(&config.gvproxy_path);

    let gvproxy_out =
        File::create(&config.nethelper_log_path).context("Failed to create nethelper log file")?;
    let gvproxy_err =
        File::try_clone(&gvproxy_out).context("Failed to clone nethelper log file handle")?;

    chown(
        &config.nethelper_log_path,
        Some(config.invoker_uid),
        Some(config.invoker_gid),
    )
    .with_context(|| {
        format!(
            "Failed to change owner of {}",
            config.nethelper_log_path.display()
        )
    })?;

    gvproxy_cmd
        .args(&gvproxy_args)
        .stdout(gvproxy_out)
        .stderr(gvproxy_err);

    if let (Some(uid), Some(gid)) = (config.sudo_uid, config.sudo_gid) {
        // run gvproxy with dropped privileges
        gvproxy_cmd.uid(uid).gid(gid);
    }

    let gvproxy_process = gvproxy_cmd
        .spawn()
        .context("Failed to start gvproxy process")?;

    Ok(gvproxy_process)
}

// TODO: adjust for FreeBSD with vmnet-helper (normal TCP socket instead of the gvproxy tunnel)
pub fn connect_to_vm_ctrl_socket(
    config: &Config,
    resp_timeout: Option<Duration>,
) -> anyhow::Result<UnixStream> {
    let sock_path = match config.kernel.os {
        OSType::Linux => {
            host_println!("Using vsock for VM control socket");
            &config.vsock_path
        }
        _ => {
            host_println!("Using gvproxy tunnel for VM control socket");
            &config.gvproxy_net_sock_path
        }
    };

    let mut stream = UnixStream::connect(sock_path)?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    stream.set_read_timeout(resp_timeout)?;

    if config.kernel.os != OSType::Linux {
        // vsock only available for Linux VMs, use gvproxy tcp tunnel instead
        let tunnel_req = format!(
            "POST /tunnel?ip={VM_IP}&port={VM_CTRL_PORT} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n"
        );

        stream.write_all(tunnel_req.as_bytes())?;
        stream.flush()?;

        let mut resp = [0; 2];
        stream.read_exact(&mut resp)?;
        if &resp != b"OK" {
            return Err(anyhow!("Failed to establish VM control socket tunnel"));
        }
    }

    Ok(stream)
}
