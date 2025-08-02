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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ActionID(usize);

pub struct Deferred<'a> {
    actions: Vec<(ActionID, Box<dyn FnOnce() + 'a>)>,
    last_id: ActionID,
}

impl<'a> Deferred<'a> {
    pub fn new() -> Self {
        Self {
            actions: Vec::new(),
            last_id: ActionID(0),
        }
    }

    pub fn add<'b, F>(&mut self, action: F) -> ActionID
    where
        F: FnOnce() + 'b,
        'b: 'a,
    {
        let id = self.last_id;
        self.actions.push((id, Box::new(action)));
        self.last_id.0 += 1;
        id
    }

    #[allow(unused)]
    pub fn call_now(&mut self, id: ActionID) {
        if let Some((_, action)) = self.pop_action(id) {
            action();
        }
    }

    fn pop_action(&mut self, id: ActionID) -> Option<(ActionID, Box<dyn FnOnce() + 'a>)> {
        self.actions
            .iter()
            .position(|(i, _)| *i == id)
            .map(|idx| self.actions.remove(idx))
    }

    pub fn remove(&mut self, id: ActionID) -> bool {
        self.pop_action(id).is_some()
    }

    pub fn remove_all(&mut self) {
        self.actions.clear();
    }
}

impl<'a> Drop for Deferred<'a> {
    fn drop(&mut self) {
        for (_id, action) in self.actions.drain(..).rev() {
            action();
        }
    }
}
