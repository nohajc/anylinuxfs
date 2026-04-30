//! mDNS (Bonjour) registration for the VM's `.local` hostname. macOS-only —
//! the mDNS record gives the VM an addressable name on the loopback interface
//! so that the kernel's NFS client can hold a stable hostname-based mount even
//! across short network blips. Linux skips this and uses the raw IP.

use anyhow::Context;
use dns_sd::{DNSRecord, DNSService};

use crate::netutil::Host;

/// Register an A/AAAA record for `<hostname>.local` pointing at `vm_ip` on the
/// loopback. Returns the record (which must outlive the mount; dropping it
/// removes the registration) and the `Host` to use for NFS mounts (the FQDN
/// when registration succeeded, the raw IP as a fallback).
pub fn register_vm_record(
    hostname: &str,
    vm_ip: Host,
) -> anyhow::Result<(Option<DNSRecord>, Host)> {
    let fqdn = format!("{}.local", hostname);
    let conn = DNSService::create_connection().context("DNS service connection failed")?;
    let dns_rec: Option<DNSRecord> = conn
        .register_record(&fqdn, vm_ip.with_port(0)?, Some("lo0"))
        .inspect_err(|e| eprintln!("DNS registration error: {e}"))
        .ok();
    let host = if dns_rec.is_some() {
        Host::new(&fqdn)
    } else {
        vm_ip
    };
    Ok((dns_rec, host))
}
