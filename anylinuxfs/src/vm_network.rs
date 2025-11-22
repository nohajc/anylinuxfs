use std::{
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::{net::UnixStream, process::CommandExt},
    process::{Child, Command},
    time::Duration,
};

use crate::settings::{Config, Preferences};
use anyhow::{Context, anyhow};
use common_utils::{OSType, VM_CTRL_PORT, VM_IP, host_println};

pub fn gvproxy_cleanup(vfkit_sock_path: &str) -> anyhow::Result<()> {
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

pub fn start_gvproxy(config: &Config) -> anyhow::Result<Child> {
    gvproxy_cleanup(&config.vfkit_sock_path)?;

    let net_sock_uri = format!("unix://{}", &config.gvproxy_net_sock_path);
    let vfkit_sock_uri = format!("unixgram://{}", &config.vfkit_sock_path);
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
        File::create(&config.gvproxy_log_path).context("Failed to create gvproxy.log file")?;
    let gvproxy_err =
        File::try_clone(&gvproxy_out).context("Failed to clone gvproxy.log file handle")?;

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

pub fn connect_to_vm_ctrl_socket(config: &Config) -> anyhow::Result<UnixStream> {
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
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;

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
