use libc::{c_char, c_int, c_uint};
use os_socketaddr::OsSocketAddr;

#[cfg(target_os = "macos")]
mod darwin;
#[cfg(target_os = "linux")]
mod linux;

#[repr(C)]
pub struct Rpcent {
    pub r_name: *mut c_char,
    pub r_aliases: *mut *mut c_char,
    pub r_number: c_int,
}

#[repr(C)]
pub struct Rpcb {
    pub r_prog: c_uint,
    pub r_vers: c_uint,
    pub r_netid: *mut c_char,
    pub r_addr: *mut c_char,
    pub r_owner: *mut c_char,
}

#[repr(C)]
pub struct Rpcblist {
    pub rpcb_map: Rpcb,
    pub rpcb_next: *mut Rpcblist,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub prog: c_uint,
    pub vers: c_uint,
    pub netid: String,
    pub addr: OsSocketAddr,
    pub owner: String,
}

/* Standard RPC program numbers */
pub const RPCPROG_NFS: c_uint = 100003;
pub const RPCPROG_MNT: c_uint = 100005;
pub const RPCPROG_STAT: c_uint = 100024;

/// Helper wrappers that mirror the behavior of the original C `register_services`.
pub mod services {
    use std::ffi::CStr;
    use std::net::{IpAddr, SocketAddr};

    use super::*;

    const NFS_PORT: u16 = 2049;
    const MOUNT_PORT: u16 = 32767;
    const STAT_PORT: u16 = 32765;

    // Per-platform implementations of rpcb_set_entry / unregister / list.
    #[cfg(target_os = "macos")]
    use super::darwin::{getrpcbynumber, rpcb_set_entry};
    #[cfg(target_os = "macos")]
    pub use super::darwin::{list, unregister};

    #[cfg(target_os = "linux")]
    use super::linux::{getrpcbynumber, rpcb_set_entry};
    #[cfg(target_os = "linux")]
    pub use super::linux::{list, unregister};

    pub fn rpcb_set_entries(entries: &[Entry]) -> anyhow::Result<()> {
        for entry in entries {
            rpcb_set_entry(entry)?;
        }
        Ok(())
    }

    /// Register NFS, MOUNT and STAT services.
    ///
    /// Individual rpcb_set failures are logged and tolerated (e.g. on Linux
    /// the host's rpc.statd already owns the STAT entries and rpcbind refuses
    /// to overwrite them). Bailing on the first conflict would leave NFS and
    /// MOUNT only partially registered, which would then break mount.nfs
    /// lookups. We only return an error if *no* NFS entry got registered.
    pub fn register() -> anyhow::Result<()> {
        let ip_props = [("", IpAddr::from([0; 4])), ("6", IpAddr::from([0; 16]))];
        let progs = [
            (RPCPROG_NFS, NFS_PORT, vec![3, 4]),
            (RPCPROG_MNT, MOUNT_PORT, vec![1, 2, 3]),
            (RPCPROG_STAT, STAT_PORT, vec![1]),
        ];

        let mut nfs_ok = false;
        for (prog, port, versions) in progs {
            for proto in ["udp", "tcp"] {
                for (ip_suffix, ip_any_addr) in ip_props {
                    for vers in versions.iter().cloned() {
                        if prog == RPCPROG_NFS && vers == 4 && proto == "udp" {
                            // NFSv4 doesn't support UDP
                            continue;
                        }
                        let e = Entry {
                            prog,
                            vers,
                            netid: format!("{}{}", proto, ip_suffix),
                            addr: SocketAddr::new(ip_any_addr, port).into(),
                            owner: "".into(),
                        };
                        match rpcb_set_entry(&e) {
                            Ok(()) => {
                                if prog == RPCPROG_NFS {
                                    nfs_ok = true;
                                }
                            }
                            Err(err) => {
                                eprintln!(
                                    "rpcbind: register {}/v{}/{} failed: {:#}",
                                    prog, vers, e.netid, err
                                );
                            }
                        }
                    }
                }
            }
        }

        if !nfs_ok {
            anyhow::bail!("failed to register any NFS entry with rpcbind");
        }
        Ok(())
    }

    pub(super) unsafe fn collect_rpcblist_entries(
        head: *mut Rpcblist,
    ) -> anyhow::Result<Vec<Entry>> {
        if head.is_null() {
            return Ok(Vec::new());
        }
        let mut res = Vec::new();
        let mut cur = head;
        while !cur.is_null() {
            let map = unsafe { &(*cur).rpcb_map };
            let prog = map.r_prog;
            let vers = map.r_vers;

            let netid = if map.r_netid.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(map.r_netid).to_string_lossy().into_owned() }
            };
            let addr_string = if map.r_addr.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(map.r_addr).to_string_lossy().into_owned() }
            };
            let owner = if map.r_owner.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(map.r_owner).to_string_lossy().into_owned() }
            };

            if netid.starts_with("tcp") || netid.starts_with("udp") {
                let addr = parse_rpcb_addr(&addr_string).into();
                res.push(Entry {
                    prog,
                    vers,
                    netid,
                    addr,
                    owner,
                });
            }
            cur = unsafe { (*cur).rpcb_next };
        }
        Ok(res)
    }

    #[derive(Debug, Clone)]
    pub(super) enum RpcStatus {
        Success,
        Failure(Option<String>),
    }

    pub(super) fn handle_rpc_error(
        prog: c_uint,
        vers: c_uint,
        nettype: &str,
        stat: RpcStatus,
    ) -> anyhow::Result<()> {
        if let RpcStatus::Failure(msg) = stat {
            let rpc = unsafe { getrpcbynumber(prog as c_int) };
            let svc = if rpc.is_null() || unsafe { (*rpc).r_name.is_null() } {
                "-".to_string()
            } else {
                unsafe { CStr::from_ptr((*rpc).r_name).to_string_lossy().into_owned() }
            };
            let svc_str = format!("{}/v{}/{}", svc, vers, nettype);
            match msg {
                Some(m) => anyhow::bail!("{} RPC call failed: {}", svc_str, m),
                None => anyhow::bail!("{} RPC call failed", svc_str),
            }
        }
        Ok(())
    }

    fn parse_rpcb_addr(addr: &str) -> SocketAddr {
        // IPv4 example (last two digits are port octets): 0.0.0.0.3.132
        // IPv6 example (last two digits are port octets): ::.3.127

        // find position of the second last dot
        let second_last_dot = addr
            .rfind('.')
            .and_then(|pos| addr[..pos].rfind('.'))
            .unwrap_or(0);

        // extract the last two octets
        let last_two = &addr[second_last_dot..];
        let port = last_two.split('.').fold(0, |acc, octet| {
            acc * 256 + octet.parse::<u16>().unwrap_or(0)
        });

        // parse the IP address and port
        let ip: IpAddr = addr[..second_last_dot]
            .parse()
            .unwrap_or(IpAddr::from([0, 0, 0, 0]));

        SocketAddr::from((ip, port))
    }
}
