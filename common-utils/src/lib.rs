use anyhow::Context;
use std::{io, process::Child, time::Duration};
use wait_timeout::ChildExt;

pub mod log;

pub fn wait_for_child(
    child: &mut Child,
    child_name: &str,
    log_prefix: Option<log::Prefix>,
) -> anyhow::Result<()> {
    // Wait for child process to exit
    let child_status = child
        .wait_timeout(Duration::from_secs(5))
        .context(format!("Failed to wait for {child_name} with timeout"))?;
    match child_status {
        Some(status) => status.code(),
        None => {
            // Send SIGKILL to child process
            prefix_println!(
                log_prefix,
                "timeout reached, force killing {} process",
                child_name
            );
            child.kill()?;
            child.wait()?.code()
        }
    }
    .map(|s| prefix_println!(log_prefix, "{} exited with status: {}", child_name, s));

    Ok(())
}

pub fn terminate_child(
    child: &mut Child,
    child_name: &str,
    log_prefix: Option<log::Prefix>,
) -> anyhow::Result<()> {
    // Terminate child process
    if unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) } < 0 {
        return Err(io::Error::last_os_error())
            .context(format!("Failed to send SIGTERM to {child_name}"));
    }

    wait_for_child(child, child_name, log_prefix)
}
