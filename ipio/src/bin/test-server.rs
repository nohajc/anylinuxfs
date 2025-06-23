use std::{fs, process::Command, thread};

use anyhow::Context;
use ipio::ServerBuilder;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let disk_ident = args
        .iter()
        .find(|arg| arg.starts_with("custom:"))
        .context("Disk identifier not provided")?;

    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(format!(
            "/dev/r{}",
            disk_ident.trim_start_matches("custom:")
        ))
        .context("Failed to open file")?;

    let server_builder = ServerBuilder::new(4194304).context("Failed to create server builder")?;
    let args = args
        .iter()
        .skip(1)
        .map(|s| {
            if s.starts_with("custom:") {
                format!("custom:{}", server_builder.conn_string())
            } else {
                s.clone()
            }
        })
        .collect::<Vec<_>>();

    // println!("args: {:?}", args);

    let mut cmd = Command::new("bin/anylinuxfs");
    cmd.args(args);
    let (mut child, mut server) = server_builder.spawn_client(cmd)?;
    let hnd = thread::spawn(move || {
        server.serve(file).unwrap();
    });

    let status = child.wait().context("Failed to wait for child process")?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "Child process exited with status: {}",
            status
        ));
    }
    hnd.join().unwrap();

    Ok(())
}
