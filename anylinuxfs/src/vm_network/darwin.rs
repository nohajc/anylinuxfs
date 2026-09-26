use std::env;
use std::fs::File;
use std::io::BufReader;
use std::net::{IpAddr, Ipv4Addr};
use std::process::{Child, Command, Stdio};

use anyhow::Context;
use common_utils::{
    Deferred, OSType, VMNET_PREFIX_LEN, host_eprintln, host_println, terminate_child,
};
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
    vmnet_write_max_packets: Option<u32>,
    vmnet_read_max_packets: Option<u32>,
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

const MACOS_OFFLOAD_MIN_VER: Versioning = Versioning::Ideal(SemVer {
    major: 26,
    minor: 2,
    patch: 0,
    pre_rel: None,
    meta: None,
});

fn macos_version_at_least(min: Versioning) -> bool {
    if let Ok(OsVersion::MacOS(MacOS { version })) = os_version::detect() {
        Versioning::new(version).unwrap_or_default() >= min
    } else {
        false
    }
}

/// Returns true on macOS Tahoe (26.0) or later, where vmnet-helper can run
/// without root.
pub fn macos_rootless_vmnet() -> bool {
    macos_version_at_least(MACOS_TAHOE_MIN_VER)
}

/// Returns true on macOS 26.2 or later, where vmnet TSO/checksum offloading is
/// supported. Both vmnet-helper and libkrun must agree on this.
pub fn macos_vmnet_offloading_supported() -> bool {
    macos_version_at_least(MACOS_OFFLOAD_MIN_VER)
}

pub fn start_vmnet_helper(config: &Config) -> anyhow::Result<NetHelperService> {
    vfkit_sock_cleanup(&config.network.unixgram_sock_path)?;

    let rootless = macos_rootless_vmnet();
    // host_println!("vmnet-helper rootless mode: {}", rootless);

    let need_elevation =
        !rootless && config.privilege.sudo_uid.is_none() && config.privilege.invoker_uid != 0;
    if need_elevation {
        anyhow::bail!(
            "anylinuxfs is configured to use vmnet-helper which needs sudo unless you're on macOS Tahoe or later"
        );
    }

    let known_networks =
        netutil::get_interface_networks().context("Failed to get interface networks")?;

    let vmnet_pool = config.preferences.vmnet_pool();
    let random_vmnet_cidr = env::var("ANYLINUXFS_RANDOM_VMNET_CIDR").as_deref() == Ok("1");
    let vmnet_cidr = if random_vmnet_cidr {
        host_println!("Random vmnet CIDR selection enabled");
        match netutil::pick_random_available_network_in_pool(
            VMNET_PREFIX_LEN,
            &known_networks,
            vmnet_pool,
            1024,
        )
        .context("Failed to randomly select vmnet network for vmnet-helper")?
        {
            Some(cidr) => cidr,
            None => {
                host_println!("Random vmnet CIDR selection exhausted; using deterministic picker");
                netutil::pick_available_network_in_pool(
                    VMNET_PREFIX_LEN,
                    &known_networks,
                    vmnet_pool,
                )
                .context("Failed to find available network for vmnet-helper")?
            }
        }
    } else {
        netutil::pick_available_network_in_pool(VMNET_PREFIX_LEN, &known_networks, vmnet_pool)
            .context("Failed to find available network for vmnet-helper")?
    };
    host_println!("Selected vmnet CIDR: {}", vmnet_cidr);

    let mut vmnet_helper_cmd = Command::new(&config.paths.vmnet_helper_path);

    let vmnet_helper_err = File::create(&config.logs.nethelper_log_path)
        .context("Failed to create vmnet-helper.log file")?;

    privilege::chown_to_invoker(
        &config.logs.nethelper_log_path,
        config.privilege.invoker_uid,
        config.privilege.invoker_gid,
    )?;

    vmnet_helper_cmd
        .arg("--socket")
        .arg(&config.network.unixgram_sock_path);
    let offloading = config.network.vmnet_offloading && config.kernel.os == OSType::Linux;
    host_println!(
        "Starting vmnet-helper: guest_os={:?}, offloading={}",
        config.kernel.os,
        offloading
    );
    if offloading {
        vmnet_helper_cmd.args(["--enable-tso", "--enable-checksum-offload"]);
    }
    vmnet_helper_cmd
        .args([
            &format!("--start-address={}", vmnet_cidr.hosts().next().unwrap()),
            &format!("--end-address={}", vmnet_cidr.hosts().last().unwrap()),
            &format!("--subnet-mask={}", vmnet_cidr.netmask()),
            "--operation-mode=shared",
        ])
        .stdout(Stdio::piped())
        .stderr(vmnet_helper_err);

    // run vmnet-helper with dropped privileges (only on macOS Tahoe+ rootless mode)
    if rootless {
        privilege::run_as_invoker(
            &mut vmnet_helper_cmd,
            config.privilege.sudo_uid,
            config.privilege.sudo_gid,
        );
    }

    let mut vmnet_helper_process = vmnet_helper_cmd
        .spawn()
        .context("Failed to start vmnet-helper process")?;

    let _helper_output = read_vmnet_config(
        &mut vmnet_helper_process,
        &config.network.unixgram_sock_path,
    )?;

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

fn read_vmnet_config(child: &mut Child, socket_path: &str) -> anyhow::Result<VmnetConfigJson> {
    let stdout = child.stdout.take();
    let mut cleanup = Deferred::new();
    cleanup.add(|| {
        match child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) => {
                if let Err(e) = terminate_child(child, "vmnet-helper", None) {
                    host_eprintln!("{:#}", e);
                }
            }
            Err(e) => host_eprintln!("Failed to check vmnet-helper exit status: {}", e),
        }
        if let Err(e) = vfkit_sock_cleanup(socket_path) {
            host_eprintln!("{:#}", e);
        }
    });

    let child_out = BufReader::new(stdout.context("Failed to capture vmnet-helper stdout")?);
    let mut config_de = Deserializer::from_reader(child_out);
    let output = VmnetConfigJson::deserialize(&mut config_de)
        .context("Failed to parse vmnet-helper config")?;

    cleanup.remove_all();
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_config_terminates_helper() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "printf '{}\\n'; exec /bin/sleep 30"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();

        let result = read_vmnet_config(&mut child, "");
        let status = child.try_wait();
        let _ = child.kill();
        let _ = child.wait();

        assert!(result.is_err());
        assert!(status.unwrap().is_some(), "helper was not terminated");
    }
}
