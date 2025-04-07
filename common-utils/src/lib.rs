use anyhow::Context;
use std::{io, process::Child, time::Duration};
use wait_timeout::ChildExt;

pub mod log;

pub fn wait_for_child(child: &mut Child, child_name: &str) -> anyhow::Result<()> {
    // Wait for child process to exit
    let child_status = child.wait_timeout(Duration::from_secs(5))?;
    match child_status {
        Some(status) => status.code(),
        None => {
            // Send SIGKILL to child process
            host_println!("timeout reached, force killing {child_name} process");
            child.kill()?;
            child.wait()?.code()
        }
    }
    .map(|s| host_println!("{} exited with status: {}", child_name, s));

    Ok(())
}

pub fn terminate_child(child: &mut Child, child_name: &str) -> anyhow::Result<()> {
    // Terminate child process
    if unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) } < 0 {
        return Err(io::Error::last_os_error())
            .context(format!("Failed to send SIGTERM to {child_name}"));
    }

    wait_for_child(child, child_name)
}
