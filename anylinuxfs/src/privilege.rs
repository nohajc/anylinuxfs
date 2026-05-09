use anyhow::Context;
use std::io;
use std::os::unix::fs::chown;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

// Long-term privilege drop intended for the surviving daemon after mount.
//
// macOS: full setuid/setgid — the saved uid is also cleared, so this is a
// permanent, irreversible drop. macOS gvproxy and the vsock control socket
// were started under the invoker's uid, so cleanup doesn't need root.
//
// Linux: effective-only drop (seteuid/setegid). Real and saved uid stay at
// 0 because several deferred cleanups still need root: SIGTERM to a
// root-owned gvproxy (it must run as root to bind port 111), removal of
// libkrun's root:root vsock socket under /tmp's sticky bit, and the
// auto-created /mnt/<name>. Those callers wrap with EffectiveRootGuard /
// ElevateOnDrop to re-elevate just for the privileged op. The trade-off is
// that the daemon retains the *capability* to regain root; that's an
// acceptable cost on a sudo-launched VM-supervisor process.
pub(crate) fn drop_privileges(
    sudo_uid: Option<libc::uid_t>,
    sudo_gid: Option<libc::gid_t>,
) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        if let (Some(sudo_uid), Some(sudo_gid)) = (sudo_uid, sudo_gid) {
            {
                if unsafe { libc::setgid(sudo_gid) } < 0 {
                    return Err(io::Error::last_os_error()).context("Failed to setgid");
                }
                if unsafe { libc::setuid(sudo_uid) } < 0 {
                    return Err(io::Error::last_os_error()).context("Failed to setuid");
                }
            }
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    drop_effective_privileges(sudo_uid, sudo_gid)
}

// Effective-only privilege drop, paired with `elevate_effective_privileges`.
// Real and saved uid/gid stay at root, so callers can later re-elevate via
// seteuid(0) / EffectiveRootGuard. Used around code that should run as the
// invoker (DNS record creation, etc.) inside a session that still needs to
// reclaim root afterwards. Identical on macOS and Linux.
pub(crate) fn drop_effective_privileges(
    sudo_uid: Option<libc::uid_t>,
    sudo_gid: Option<libc::gid_t>,
) -> anyhow::Result<()> {
    if let (Some(sudo_uid), Some(sudo_gid)) = (sudo_uid, sudo_gid) {
        if unsafe { libc::setegid(sudo_gid) } < 0 {
            return Err(io::Error::last_os_error()).context("Failed to setegid");
        }
        if unsafe { libc::seteuid(sudo_uid) } < 0 {
            return Err(io::Error::last_os_error()).context("Failed to seteuid");
        }
    }
    Ok(())
}

// Re-elevate effective uid/gid to the real (process) values. Only succeeds
// if the saved uid/gid are root — i.e. only after `drop_effective_privileges`
// (or after `drop_privileges` on Linux, which preserves saved uid; not after
// macOS `drop_privileges`, which clears it).
pub(crate) fn elevate_effective_privileges() -> anyhow::Result<()> {
    let real_uid = unsafe { libc::getuid() };
    let real_gid = unsafe { libc::getgid() };
    if unsafe { libc::seteuid(real_uid) } < 0 {
        return Err(io::Error::last_os_error()).context("Failed to seteuid");
    }
    if unsafe { libc::setegid(real_gid) } < 0 {
        return Err(io::Error::last_os_error()).context("Failed to setegid");
    }
    Ok(())
}

/// RAII guard: elevate effective uid/gid to the saved real-uid-root values on
/// acquire, drop back to invoker on release. Used to open root-only resources
/// (e.g. the libkrun-created vsock socket) from threads that otherwise run
/// with dropped effective privileges. No-op on macOS / non-sudo invocations.
#[cfg(target_os = "linux")]
pub(crate) struct EffectiveRootGuard {
    prev_uid: Option<libc::uid_t>,
    prev_gid: Option<libc::gid_t>,
}

#[cfg(target_os = "linux")]
impl EffectiveRootGuard {
    pub(crate) fn acquire() -> Self {
        let prev_uid = unsafe { libc::geteuid() };
        let prev_gid = unsafe { libc::getegid() };
        // Only elevate if our real uid is root (i.e. we were sudo'd). seteuid(0)
        // fails with EPERM otherwise, and there's nothing to guard.
        let is_real_root = unsafe { libc::getuid() } == 0;
        let (set_uid, set_gid) = if is_real_root && prev_uid != 0 {
            let _ = unsafe { libc::seteuid(0) };
            let _ = unsafe { libc::setegid(0) };
            (Some(prev_uid), Some(prev_gid))
        } else {
            (None, None)
        };
        Self {
            prev_uid: set_uid,
            prev_gid: set_gid,
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for EffectiveRootGuard {
    fn drop(&mut self) {
        if let Some(gid) = self.prev_gid {
            unsafe { libc::setegid(gid) };
        }
        if let Some(uid) = self.prev_uid {
            unsafe { libc::seteuid(uid) };
        }
    }
}

/// Marker whose Drop re-elevates effective uid/gid to 0. Used to ensure
/// deferred cleanups in `run_mount_child` run as root after the long-running
/// parent has dropped effective privileges to the invoker. Requires
/// drop_privileges to have used seteuid/setegid (saved uid still 0).
#[cfg(target_os = "linux")]
pub(crate) struct ElevateOnDrop;

#[cfg(target_os = "linux")]
impl Drop for ElevateOnDrop {
    fn drop(&mut self) {
        if unsafe { libc::getuid() } == 0 {
            unsafe { libc::seteuid(0) };
            unsafe { libc::setegid(0) };
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn chown_tree_to_invoker(
    path: &Path,
    uid: libc::uid_t,
    gid: libc::gid_t,
) -> anyhow::Result<()> {
    use std::fs;
    chown(path, Some(uid), Some(gid)).with_context(|| format!("chown {}", path.display()))?;
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            chown_tree_to_invoker(&entry.path(), uid, gid)?;
        }
    }
    Ok(())
}

/// Chown `path` to the invoker's uid/gid with a uniform error message.
/// Replaces the recurring `chown(path, Some(uid), Some(gid)).with_context(…)?`
/// boilerplate at every site that creates a host-side artifact (sockets, log
/// files, mount points) needing invoker ownership after a sudo'd run.
pub(crate) fn chown_to_invoker(
    path: impl AsRef<Path>,
    uid: libc::uid_t,
    gid: libc::gid_t,
) -> anyhow::Result<()> {
    let path = path.as_ref();
    chown(path, Some(uid), Some(gid))
        .with_context(|| format!("Failed to change owner of {}", path.display()))
}

/// Configure `cmd` to spawn as the invoker (the user behind `sudo`) when
/// privilege-drop info is available. No-op when `sudo_uid`/`sudo_gid` are
/// `None` — i.e. the process wasn't sudo'd in the first place.
pub(crate) fn run_as_invoker(
    cmd: &mut Command,
    sudo_uid: Option<libc::uid_t>,
    sudo_gid: Option<libc::gid_t>,
) {
    if let (Some(uid), Some(gid)) = (sudo_uid, sudo_gid) {
        cmd.uid(uid).gid(gid);
    }
}

/// Drop the libkrun guest-VM privileges via krun_setuid/krun_setgid. macOS
/// only — Linux keeps libkrun running as root because `/dev/kvm` and
/// `/dev/vhost-*` need it (mode 660, group=kvm) and dropping privileges
/// inside the VMM has been observed to break startup. Linux call is a
/// no-op so the cfg gate disappears from the orchestration site.
pub(crate) fn apply_krun_priv_drop(
    _ctx_id: u32,
    _config: &crate::settings::Config,
) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use crate::ResultWithCtx;
        if let Some(uid) = _config.privilege.sudo_uid {
            krun::krun_setuid(_ctx_id, uid).context("Failed to set vmm uid")?;
        }
        if let Some(gid) = _config.privilege.sudo_gid {
            krun::krun_setgid(_ctx_id, gid).context("Failed to set vmm gid")?;
        }
    }
    Ok(())
}

/// The resolved invoking user's uid and gid. When running as root via sudo
/// (or a process-tree fallback), these hold the non-root user behind the
/// elevation. When not running as root, they equal the current process uid/gid.
pub(crate) struct InvokerIdentity {
    pub uid: libc::uid_t,
    pub gid: libc::gid_t,
}

struct ProcInfo {
    uid: libc::uid_t,
    gid: libc::gid_t,
    ppid: libc::pid_t,
}

#[cfg(target_os = "linux")]
fn get_proc_info(pid: libc::pid_t) -> Option<ProcInfo> {
    let status = procfs::process::Process::new(pid).ok()?.status().ok()?;
    Some(ProcInfo {
        uid: status.ruid,
        gid: status.rgid,
        ppid: status.ppid as libc::pid_t,
    })
}

#[cfg(target_os = "macos")]
fn get_proc_info(pid: libc::pid_t) -> Option<ProcInfo> {
    use std::mem;
    let mut info = unsafe { mem::zeroed::<libc::proc_bsdinfo>() };
    let ret = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            mem::size_of::<libc::proc_bsdinfo>() as libc::c_int,
        )
    };
    if ret <= 0 {
        return None;
    }
    Some(ProcInfo {
        uid: info.pbi_ruid,
        gid: info.pbi_rgid,
        ppid: info.pbi_ppid as libc::pid_t,
    })
}

fn walk_to_invoker() -> anyhow::Result<InvokerIdentity> {
    std::iter::successors(get_proc_info(unsafe { libc::getpid() }), |info| {
        get_proc_info(info.ppid)
    })
    .skip(1)
    .take(5)
    .find_map(|info| {
        (info.uid != 0).then_some(InvokerIdentity {
            uid: info.uid,
            gid: info.gid,
        })
    })
    .ok_or_else(|| {
        anyhow::anyhow!("This program must not be run directly by root; use sudo instead")
    })
}

/// Resolve the identity of the invoking user.
///
/// Resolution order (when running as root):
/// 1. `SUDO_UID` + `SUDO_GID` environment variables (standard sudo)
/// 2. Process-tree walk: ascend up to 5 ancestors via [`get_proc_info`],
///    returning the first non-root real uid/gid found.
///
/// Returns immediately with the current process uid/gid when not running as root.
pub(crate) fn resolve_invoker_identity() -> anyhow::Result<InvokerIdentity> {
    let uid = unsafe { libc::getuid() };
    if uid != 0 {
        return Ok(InvokerIdentity {
            uid,
            gid: unsafe { libc::getgid() },
        });
    }

    let sudo_uid = std::env::var("SUDO_UID")
        .ok()
        .and_then(|s| s.parse::<libc::uid_t>().ok());
    let sudo_gid = std::env::var("SUDO_GID")
        .ok()
        .and_then(|s| s.parse::<libc::gid_t>().ok());

    if let (Some(uid), Some(gid)) = (sudo_uid, sudo_gid) {
        return Ok(InvokerIdentity { uid, gid });
    }

    walk_to_invoker()
}
