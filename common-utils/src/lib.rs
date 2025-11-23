use anyhow::Context;
use bstr::{BStr, BString, ByteSlice};
use clap::ValueEnum;
use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};
use serde::{Deserialize, Serialize};
use std::{ffi::CString, io, os::unix::ffi::OsStrExt, path::Path, process::Child, time::Duration};
use wait_timeout::ChildExt;

pub mod log;

pub const VM_GATEWAY_IP: &str = "192.168.127.1";
pub const VM_IP: &str = "192.168.127.2";
pub const VM_CTRL_PORT: u16 = 7350;

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

#[derive(
    Clone, Copy, ValueEnum, Debug, PartialEq, Eq, PartialOrd, Ord, Default, Deserialize, Serialize,
)]
pub enum OSType {
    #[clap(name = "linux")]
    #[default]
    Linux,
    #[clap(name = "freebsd")]
    FreeBSD,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CustomActionConfig {
    #[serde(default)]
    description: String,
    #[serde(default)]
    before_mount: String,
    #[serde(default)]
    after_mount: String,
    #[serde(default)]
    before_unmount: String,
    #[serde(default)]
    environment: Vec<String>, // KEY=value format
    #[serde(default)]
    capture_environment: Vec<String>,
    #[serde(default)]
    override_nfs_export: String,
    required_os: Option<OSType>,
}

impl CustomActionConfig {
    pub const VM_EXPORTED_VARS: &[&[u8]] = &[b"ALFS_VM_MOUNT_POINT"];

    pub fn all_scripts(&self) -> [&str; 3] {
        [&self.before_mount, &self.after_mount, &self.before_unmount]
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn before_mount(&self) -> &str {
        &self.before_mount
    }

    pub fn after_mount(&self) -> &str {
        &self.after_mount
    }

    pub fn before_unmount(&self) -> &str {
        &self.before_unmount
    }

    pub fn environment(&self) -> impl Iterator<Item = &BStr> {
        self.environment.iter().map(|e| BStr::new(e))
    }

    pub fn capture_environment(&self) -> impl Iterator<Item = &BStr> {
        self.capture_environment.iter().map(|e| BStr::new(e))
    }

    pub fn override_nfs_export(&self) -> &str {
        &self.override_nfs_export
    }

    pub fn required_os(&self) -> Option<OSType> {
        self.required_os
    }

    const PERCENT_ENCODE_SET: &AsciiSet = &CONTROLS.add(b' ');

    pub fn percent_encode(&self) -> anyhow::Result<String> {
        let ron_encoded = ron::ser::to_string(&self)?;
        Ok(utf8_percent_encode(&ron_encoded, Self::PERCENT_ENCODE_SET).to_string())
    }

    pub fn percent_decode(encoded: &str) -> anyhow::Result<Self> {
        let decoded = percent_decode_str(encoded).decode_utf8()?;
        Ok(ron::de::from_str(&decoded)?)
    }
}

// FIXME: remove at some point in the future
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CustomActionConfigOld {
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

impl From<CustomActionConfigOld> for CustomActionConfig {
    fn from(old: CustomActionConfigOld) -> Self {
        Self {
            description: old.description.to_str_lossy().into(),
            before_mount: old.before_mount.to_str_lossy().into(),
            after_mount: old.after_mount.to_str_lossy().into(),
            before_unmount: old.before_unmount.to_str_lossy().into(),
            environment: old
                .environment
                .into_iter()
                .map(|e| e.to_str_lossy().into())
                .collect(),
            capture_environment: old
                .capture_environment
                .into_iter()
                .map(|e| e.to_str_lossy().into())
                .collect(),
            override_nfs_export: old.override_nfs_export,
            required_os: None,
        }
    }
}

pub trait FromPath {
    fn from_path(path: impl AsRef<Path>) -> Self;
}

impl FromPath for CString {
    fn from_path(path: impl AsRef<Path>) -> Self {
        CString::new(path.as_ref().as_bytes()).unwrap()
    }
}

pub trait PathExt {
    fn as_bytes(&self) -> &[u8];
    fn from_bytes(bstr: &[u8]) -> &Self;
}

impl PathExt for Path {
    fn as_bytes(&self) -> &[u8] {
        self.as_os_str().as_bytes()
    }

    fn from_bytes(bstr: &[u8]) -> &Self {
        bstr.to_path().unwrap()
    }
}
