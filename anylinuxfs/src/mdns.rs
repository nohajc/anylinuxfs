//! mDNS (Bonjour) registration for the VM's `.local` hostname.
//!
//! On macOS we register an A/AAAA record on `lo0` so the kernel's NFS client
//! can hold a stable hostname-based mount even across short network blips.
//!
//! On Linux there is currently no equivalent — the Avahi daemon isn't
//! universally available and host mounting works fine over the raw IP. The
//! `Registration` type below is a no-op stub on Linux; it exists so the
//! cross-platform call sites in `cmd_mount.rs` don't need cfg gates around
//! the registration handle. If we ever want Avahi-based registration on
//! Linux this is where it lands.

#[cfg(target_os = "macos")]
use anyhow::Context;
#[cfg(target_os = "macos")]
use dns_sd::{DNSRecord, DNSService};

use crate::netutil::Host;

/// Holds the state that keeps an mDNS registration alive for the duration of
/// a mount. On macOS this is the `DNSService` connection plus the registered
/// `DNSRecord`; dropping the connection unregisters every record allocated
/// through it. On Linux this is an empty marker — see the module docs.
pub struct Registration {
    #[cfg(target_os = "macos")]
    _conn: DNSService,
    #[cfg(target_os = "macos")]
    record: Option<DNSRecord>,
}

impl Registration {
    /// Block until the OS has acknowledged the registration. macOS-only;
    /// no-op on Linux because there's no registration to wait for.
    pub fn wait_committed(&mut self) -> anyhow::Result<()> {
        #[cfg(target_os = "macos")]
        if let Some(rec) = self.record.as_mut() {
            rec.wait_for_registration()
                .context("Could not set DNS record for the VM")?;
        }
        Ok(())
    }
}

/// Register an A/AAAA record for `<hostname>.local` pointing at `vm_ip` on
/// the loopback (macOS), or no-op (Linux). Returns the registration handle —
/// keep it bound for the lifetime of the mount; dropping it removes the
/// record on macOS — and the `Host` to use for NFS mounts (the FQDN when
/// registration succeeded on macOS, the raw IP otherwise).
#[cfg(target_os = "macos")]
pub fn register_vm_record(hostname: &str, vm_ip: Host) -> anyhow::Result<(Registration, Host)> {
    let fqdn = format!("{}.local", hostname);
    let conn = DNSService::create_connection().context("DNS service connection failed")?;
    let record: Option<DNSRecord> = conn
        .register_record(&fqdn, vm_ip.with_port(0)?, Some("lo0"))
        .inspect_err(|e| eprintln!("DNS registration error: {e}"))
        .ok();
    let host = if record.is_some() {
        Host::new(&fqdn)
    } else {
        vm_ip
    };
    Ok((
        Registration {
            _conn: conn,
            record,
        },
        host,
    ))
}

#[cfg(target_os = "linux")]
pub fn register_vm_record(_hostname: &str, vm_ip: Host) -> anyhow::Result<(Registration, Host)> {
    Ok((Registration {}, vm_ip))
}
