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
        if let Some(uid) = _config.sudo_uid {
            krun::krun_setuid(_ctx_id, uid).context("Failed to set vmm uid")?;
        }
        if let Some(gid) = _config.sudo_gid {
            krun::krun_setgid(_ctx_id, gid).context("Failed to set vmm gid")?;
        }
    }
    Ok(())
}
