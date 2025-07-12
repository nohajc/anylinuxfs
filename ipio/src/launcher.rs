use std::{
    ffi::c_void,
    process::{Command, Stdio},
    thread,
};

use anyhow::Context;

use crate::{IOCallbacks, ServerBuilder};

type Void = c_void;

#[swift_bridge::bridge]
mod ffi {
    extern "Swift" {
        fn blkdev_read(hnd: usize, buf: *mut Void, offset: i64, size: isize) -> i64;
        fn blkdev_write(hnd: usize, buf: *mut Void, offset: i64, size: isize) -> i64;
        fn blkdev_size(hnd: usize) -> i64;
    }

    extern "Rust" {
        // env - vector of key=value formatted strings
        fn run(hnd: usize, args: Vec<String>, env: Vec<String>) -> String;
    }
}

fn run(hnd: usize, args: Vec<String>, env: Vec<String>) -> String {
    // println!("Running with args: {:?}", args);
    match run_with_result(hnd, args, env) {
        Ok(out) => out,
        Err(e) => {
            format!("Error: {:#}", e)
        }
    }
}

fn run_with_result(hnd: usize, args: Vec<String>, env: Vec<String>) -> anyhow::Result<String> {
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

    let server_builder =
        ServerBuilder::new(hnd, 4194304).context("Failed to create server builder")?;
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
    cmd.args(&args);
    cmd.envs(env.iter().filter_map(|s| {
        let mut parts = s.splitn(2, '=');
        let Some(key) = parts.next() else {
            return None;
        };
        let Some(value) = parts.next() else {
            return None;
        };
        Some((key.to_string(), value.to_string()))
    }));
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = if args[0] == "stop" {
        cmd.spawn()?
    } else {
        let (child, mut server) = server_builder.spawn_client(cmd)?;
        let _hnd = thread::spawn(move || {
            server
                .serve(IOCallbacks {
                    read: ffi::blkdev_read,
                    write: ffi::blkdev_write,
                    size: ffi::blkdev_size,
                    hnd,
                })
                .unwrap();
        });
        child
    };

    // let _hnd2 = thread::spawn(move || {
    let status = child.wait()?;
    // });

    // let out = child
    //     .wait_with_output()
    //     .context("Failed to wait for child process")?;
    // if !out.status.success() {
    //     return Err(anyhow::anyhow!(
    //         "{}\n\nChild process exited with status: {}",
    //         String::from_utf8_lossy(&out.stderr),
    //         out.status
    //     ));
    // }

    // Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    Ok(status.to_string())
}
