use std::fs::File;
use std::io::BufReader;
use std::net::{IpAddr, Ipv4Addr};
use std::process::{Command, Stdio};

use anyhow::Context;
use common_utils::VMNET_PREFIX_LEN;
use ipnet::Ipv4Net;
use os_version::{MacOS, OsVersion};
use serde::Deserialize;
use serde_json::Deserializer;
use versions::{SemVer, Versioning};

use super::{NetHelperService, vfkit_sock_cleanup};
use crate::netutil::{self, Host};
use crate::privilege;
use crate::settings::{Config, Preferences};

#[allow(unused)]
#[derive(Debug, Deserialize)]
struct VmnetConfigJson {
    vmnet_write_max_packets: u32,
    vmnet_read_max_packets: u32,
    vmnet_subnet_mask: String,
    vmnet_mtu: u32,
    vmnet_end_address: String,
    vmnet_start_address: String,
    vmnet_interface_id: String,
    vmnet_max_packet_size: u32,
    vmnet_nat66_prefix: String,
    vmnet_mac_address: String,
}

struct VmnetConfig {
    _helper_output: VmnetConfigJson,
    vmnet_cidr: Ipv4Net,
}

impl VmnetConfig {
    fn vm_ip(&self) -> Ipv4Addr {
        self.vmnet_cidr.hosts().nth(1).unwrap()
    }
}

const MACOS_TAHOE_MIN_VER: Versioning = Versioning::Ideal(SemVer {
    major: 26,
    minor: 0,
    patch: 0,
    pre_rel: None,
    meta: None,
});

pub fn start_vmnet_helper(config: &Config) -> anyhow::Result<NetHelperService> {
    vfkit_sock_cleanup(&config.unixgram_sock_path)?;

    let rootless = if let OsVersion::MacOS(MacOS { version }) = os_version::detect()? {
        Versioning::new(version).unwrap_or_default() >= MACOS_TAHOE_MIN_VER
    } else {
        false
    };
    // host_println!("vmnet-helper rootless mode: {}", rootless);

    let need_elevation = !rootless && config.sudo_uid.is_none() && config.invoker_uid != 0;
    if need_elevation {
        anyhow::bail!(
            "anylinuxfs is configured to use vmnet-helper which needs sudo unless you're on macOS Tahoe or later"
        );
    }

    let known_networks =
        netutil::get_interface_networks().context("Failed to get interface networks")?;

    let vmnet_cidr = netutil::pick_available_network_in_pool(
        VMNET_PREFIX_LEN,
        &known_networks,
        config.preferences.vmnet_pool(),
    )
    .context("Failed to find available network for vmnet-helper")?;

    let mut vmnet_helper_cmd = Command::new(&config.vmnet_helper_path);

    let vmnet_helper_err = File::create(&config.nethelper_log_path)
        .context("Failed to create vmnet-helper.log file")?;

    privilege::chown_to_invoker(
        &config.nethelper_log_path,
        config.invoker_uid,
        config.invoker_gid,
    )?;

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

    // run vmnet-helper with dropped privileges (only on macOS Tahoe+ rootless mode)
    if rootless {
        privilege::run_as_invoker(&mut vmnet_helper_cmd, config.sudo_uid, config.sudo_gid);
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

    let vm_ip = vmnet_config.vm_ip();
    Ok(NetHelperService {
        proc: vmnet_helper_process,
        name: "vmnet-helper",
        vm_host_ip: Host::from_ip(IpAddr::V4(vm_ip), None),
        vm_native_cidr: Some(vmnet_config.vmnet_cidr),
        vm_native_ip: Some(vm_ip),
    })
}
