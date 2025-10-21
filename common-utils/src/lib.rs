use anyhow::Context;
use bstr::{BStr, BString, ByteSlice};
use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};
use serde::{Deserialize, Serialize};
use std::{io, process::Child, time::Duration};
use wait_timeout::ChildExt;

pub mod log;

pub fn path_safe_label_name(name: &str) -> Option<String> {
    let name_subst = name.replace("/", "-").replace(" ", "_").replace(":", "_");
    name_subst
        .chars()
        .position(|c| c != '-')
        .map(|idx| name_subst[idx..].to_string())
}

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

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CustomActionConfig {
    #[serde(default)]
    pub description: BString,
    #[serde(default)]
    pub before_mount: BString,
    #[serde(default)]
    pub after_mount: BString,
    #[serde(default)]
    pub before_unmount: BString,
    #[serde(default)]
    pub environment: Vec<BString>, // KEY=value format
    #[serde(default)]
    pub capture_environment: Vec<BString>,
    #[serde(default)]
    pub override_nfs_export: String,
}

impl CustomActionConfig {
    pub const VM_EXPORTED_VARS: &[&[u8]] = &[b"ALFS_VM_MOUNT_POINT"];

    pub fn all_scripts(&self) -> [&BStr; 3] {
        [
            self.before_mount.as_bstr(),
            self.after_mount.as_bstr(),
            self.before_unmount.as_bstr(),
        ]
    }

    const PERCENT_ENCODE_SET: &AsciiSet = &CONTROLS.add(b' ');

    pub fn percent_encode(&self) -> anyhow::Result<String> {
        let json_encoded = serde_json::to_string(&self)?;
        Ok(utf8_percent_encode(&json_encoded, Self::PERCENT_ENCODE_SET).to_string())
    }

    pub fn percent_decode(encoded: &str) -> anyhow::Result<Self> {
        let decoded = percent_decode_str(encoded).decode_utf8()?;
        Ok(serde_json::from_str(&decoded)?)
    }
}
