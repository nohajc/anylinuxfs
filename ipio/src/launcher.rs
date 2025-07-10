use std::{fs, process::Command, thread};

use anyhow::Context;

use crate::ServerBuilder;

#[swift_bridge::bridge]
mod ffi {
    extern "Rust" {
        fn run(args: Vec<String>);
    }
}

fn run(args: Vec<String>) {
    println!("Running with args: {:?}", args);
    if let Err(e) = run_with_result(args) {
        eprintln!("Error: {:#}", e);
    }
}

fn run_with_result(args: Vec<String>) -> anyhow::Result<()> {
    // test spawning a process using libc::posix_spawn
    // let mut child_pid = 0;
    // let res = unsafe {
    //     libc::posix_spawn(
    //         &mut child_pid,
    //         "Downloads/anylinuxfs\0".as_bytes().as_ptr() as *const c_char,
    //         std::ptr::null(),
    //         std::ptr::null(),
    //         std::ptr::null(),
    //         std::ptr::null(),
    //     )
    // };
    // if res != 0 {
    //     return Err(anyhow::anyhow!("Failed to spawn process: {}", res));
    // }
    // _ = unsafe { libc::waitpid(child_pid, std::ptr::null_mut(), 0) };

    let disk_ident = args
        .iter()
        .find(|arg| arg.starts_with("custom:"))
        .context("Disk identifier not provided")?;

    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(disk_ident.trim_start_matches("custom:"))
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

    let mut cmd = Command::new("Downloads/afs/bin/anylinuxfs");
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
