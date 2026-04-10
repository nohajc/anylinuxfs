use bstr::BString;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Request {
    Quit,
    SubscribeEvents,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Response {
    Ack,
    ReportEvent(Report),
    /// Structured VM event delivered over the IPC control socket.
    /// Replaces the stdout tag-scraping protocol (all tags except vmproxy-ready).
    VmEvent(VmEvent),
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Report {
    pub kernel_log: BString,
}

impl Report {
    pub fn new(kernel_log: BString) -> Self {
        Self { kernel_log }
    }
}

/// Structured VM lifecycle and metadata events sent by vmproxy to the host.
///
/// These replace the `<anylinuxfs-tag:value>` stdout protocol for all tags
/// EXCEPT `<anylinuxfs-vmproxy-ready>`, which remains a stdout signal because
/// it bootstraps the IPC connection itself (chicken-and-egg: the host must
/// receive it before it can connect to the control socket to subscribe).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum VmEvent {
    /// Detected filesystem type (replaces `<anylinuxfs-type:FS>`).
    FsType(String),
    /// Detected filesystem label (replaces `<anylinuxfs-label:LBL>`).
    FsLabel(String),
    /// NFS export path is ready (replaces `<anylinuxfs-nfs-export:PATH>`).
    /// May be emitted multiple times (once per exported path).
    NfsExport(String),
    /// Filesystem was remounted read-only (replaces `<anylinuxfs-mount:changed-to-ro>`).
    MountChangedToRo,
    /// vmproxy is about to prompt for a passphrase on the TTY
    /// (replaces `<anylinuxfs-passphrase-prompt:start>`).
    PassphrasePromptStart,
    /// Passphrase entry is complete (replaces `<anylinuxfs-passphrase-prompt:end>`).
    PassphrasePromptEnd,
    /// Host should start showing all VM log output (replaces `<anylinuxfs-force-output:on>`).
    ForceOutputOn,
    /// Host should suppress verbose VM log output (replaces `<anylinuxfs-force-output:off>`).
    ForceOutputOff,
    /// vmproxy exited with an error (replaces `<anylinuxfs-exit-code:N>`).
    ExitCode(i32),
}
