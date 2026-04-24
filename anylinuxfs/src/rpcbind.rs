use libc::{c_char, c_int, c_uint, c_void};
use os_socketaddr::OsSocketAddr;
use std::ffi::CStr;
use std::net::SocketAddr;
use std::{ffi::CString, ptr};

#[cfg(target_os = "macos")]
use libc::{sockaddr, timeval};

#[cfg(target_os = "macos")]
#[link(name = "oncrpc", kind = "framework")]
unsafe extern "C" {
    /// int rpcb_unset(const char *netid, unsigned int program, unsigned int version);
    #[link_name = "_newrpclib_rpcb_unset"]
    pub fn rpcb_unset_mac(netid: *const c_char, program: c_uint, version: c_uint) -> c_int;

    /// int rpcb_set(const char *netid, unsigned int program, unsigned int version,
    ///              const struct sockaddr *addr);
    #[link_name = "_newrpclib_rpcb_set"]
    pub fn rpcb_set_mac(
        netid: *const c_char,
        program: c_uint,
        version: c_uint,
        addr: *const sockaddr,
    ) -> bool;

    /* Additional declarations pulled from <rpc/rpc.h> and related headers */
    #[link_name = "_newrpclib_clnt_create_timeout"]
    pub fn clnt_create_timeout(
        host: *const c_char,
        prognum: c_uint,
        versnum: c_uint,
        nettype: *const c_char,
        timeout: *const timeval,
    ) -> *mut CLIENT;

    #[link_name = "_newrpclib_xdr_rpcblist_ptr"]
    pub fn xdr_rpcblist_ptr(xdrs: *mut c_void, objp: *mut c_void, len: c_uint) -> c_int;

    pub fn xdr_void() -> c_int;
    pub fn clnt_sperrno(stat: c_int) -> *const c_char;
    pub fn getrpcbynumber(number: c_int) -> *mut Rpcent;
}

// Linux uses libtirpc. Its rpcb_* API differs from macOS's oncrpc:
//   - takes `struct netconfig*` (from getnetconfigent("tcp")) instead of a netid string
//   - takes `struct netbuf*` (wraps a sockaddr buffer) instead of `struct sockaddr*`
#[cfg(not(target_os = "macos"))]
#[repr(C)]
pub struct Netbuf {
    pub maxlen: c_uint,
    pub len: c_uint,
    pub buf: *mut c_void,
}

#[cfg(not(target_os = "macos"))]
#[repr(C)]
pub struct Netconfig {
    // We only ever pass the pointer through; field layout is libtirpc-opaque
    // for our purposes. Declaring an empty struct would make Rust treat it as
    // ZST; this at least keeps the type distinct.
    _opaque: [u8; 0],
}

#[cfg(not(target_os = "macos"))]
unsafe extern "C" {
    pub fn rpcb_set(
        program: c_uint,
        version: c_uint,
        netconf: *const Netconfig,
        addr: *const Netbuf,
    ) -> c_int;

    pub fn rpcb_unset(program: c_uint, version: c_uint, netconf: *const Netconfig) -> c_int;

    pub fn rpcb_getmaps(netconf: *const Netconfig, host: *const c_char) -> *mut Rpcblist;

    pub fn getnetconfigent(netid: *const c_char) -> *mut Netconfig;
    pub fn freenetconfigent(netconf: *mut Netconfig);

    // Shared libc helpers also available on Linux
    pub fn getrpcbynumber(number: c_int) -> *mut Rpcent;
}

#[allow(non_camel_case_types)]
#[cfg(target_os = "macos")]
pub type xdrproc_t =
    unsafe extern "C" fn(xdrs: *mut c_void, addrp: *mut c_void, len: c_uint) -> c_int;

#[allow(non_camel_case_types)]
#[cfg(target_os = "macos")]
pub type xdrproc_void_t = unsafe extern "C" fn() -> c_int;

#[cfg(target_os = "macos")]
#[repr(C)]
pub struct ClntOps {
    pub cl_call: extern "C" fn(
        *mut CLIENT,
        c_uint,
        xdrproc_void_t,
        *mut c_void,
        xdrproc_t,
        *mut c_void,
        timeval,
    ) -> c_int,

    pub cl_abort: extern "C" fn(),
    pub cl_geterr: extern "C" fn(*mut CLIENT, *mut c_void),
    pub cl_freeres: extern "C" fn(*mut CLIENT, *const c_void, *mut c_void) -> c_int,
    pub cl_destroy: extern "C" fn(*mut CLIENT),
    pub cl_control: extern "C" fn(*mut CLIENT, c_int, *mut c_char) -> c_int,
}

#[cfg(target_os = "macos")]
#[repr(C)]
pub struct CLIENT {
    pub cl_auth: *mut c_void,
    pub cl_ops: *mut ClntOps,
    pub cl_private: *mut c_void,
}

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
pub const RPCPROG_RPCB: c_uint = 100000;
pub const RPCPROG_NFS: c_uint = 100003;
pub const RPCPROG_MNT: c_uint = 100005;
pub const RPCPROG_STAT: c_uint = 100024;

/// Helper wrappers that mirror the behavior of the original C `register_services`.
pub mod services {
    use std::net::IpAddr;

    use super::*;

    const NFS_PORT: u16 = 2049;
    const MOUNT_PORT: u16 = 32767;
    const STAT_PORT: u16 = 32765;

    #[cfg(target_os = "macos")]
    const RPCBVERS4: c_uint = 4;
    #[cfg(target_os = "macos")]
    const RPCBPROC_DUMP: c_uint = 4;
    const RPC_SUCCESS: c_int = 0;

    #[cfg(target_os = "macos")]
    pub fn rpcb_set_entry(entry: &Entry) -> anyhow::Result<()> {
        let c_netid = CString::new(entry.netid.as_bytes()).unwrap();
        let stat = unsafe {
            rpcb_set_mac(
                c_netid.as_ptr(),
                entry.prog,
                entry.vers,
                entry.addr.as_ptr(),
            )
        };
        handle_rpc_error(entry.prog, entry.vers, &entry.netid, stat.into())
    }

    #[cfg(not(target_os = "macos"))]
    pub fn rpcb_set_entry(entry: &Entry) -> anyhow::Result<()> {
        let c_netid = CString::new(entry.netid.as_bytes()).unwrap();
        let nc = unsafe { getnetconfigent(c_netid.as_ptr()) };
        if nc.is_null() {
            anyhow::bail!(
                "getnetconfigent returned null for netid {:?}",
                entry.netid
            );
        }
        // netbuf wraps a raw sockaddr buffer (OsSocketAddr already stores a
        // sockaddr_storage-sized buffer, so we point netbuf at it).
        let sockaddr_ptr = entry.addr.as_ptr() as *const c_void as *mut c_void;
        let sockaddr_len = entry.addr.len() as c_uint;
        let nb = Netbuf {
            maxlen: sockaddr_len,
            len: sockaddr_len,
            buf: sockaddr_ptr,
        };
        let ok = unsafe { rpcb_set(entry.prog, entry.vers, nc, &nb) };
        unsafe { freenetconfigent(nc) };
        // libtirpc's rpcb_set returns bool_t (int) — nonzero means success.
        let rpc_stat: RpcStatus = if ok != 0 {
            RpcStatus::Success
        } else {
            RpcStatus::Failure(None)
        };
        handle_rpc_error(entry.prog, entry.vers, &entry.netid, rpc_stat)
    }

    pub fn rpcb_set_entries(entries: &[Entry]) -> anyhow::Result<()> {
        for entry in entries {
            rpcb_set_entry(entry)?;
        }
        Ok(())
    }

    /// Unregister the NFS, MOUNT and STAT program/version pairs.
    #[cfg(target_os = "macos")]
    pub fn unregister() {
        unsafe {
            rpcb_unset_mac(ptr::null(), RPCPROG_NFS, 3);
            rpcb_unset_mac(ptr::null(), RPCPROG_NFS, 4);
            rpcb_unset_mac(ptr::null(), RPCPROG_MNT, 1);
            rpcb_unset_mac(ptr::null(), RPCPROG_MNT, 2);
            rpcb_unset_mac(ptr::null(), RPCPROG_MNT, 3);
            rpcb_unset_mac(ptr::null(), RPCPROG_STAT, 1);
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn unregister() {
        // libtirpc's rpcb_unset with a null netconfig unregisters all transports.
        unsafe {
            rpcb_unset(RPCPROG_NFS, 3, ptr::null());
            rpcb_unset(RPCPROG_NFS, 4, ptr::null());
            rpcb_unset(RPCPROG_MNT, 1, ptr::null());
            rpcb_unset(RPCPROG_MNT, 2, ptr::null());
            rpcb_unset(RPCPROG_MNT, 3, ptr::null());
            rpcb_unset(RPCPROG_STAT, 1, ptr::null());
        }
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

    /// List registered RPC services by querying rpcbind.
    #[cfg(target_os = "macos")]
    pub fn list() -> anyhow::Result<Vec<Entry>> {
        unsafe {
            let host = CString::new("127.0.0.1").unwrap();
            let nettype = "tcp";
            let c_nettype = CString::new(nettype).unwrap();

            let timeout_short = timeval {
                tv_sec: 5,
                tv_usec: 0,
            };
            let client = clnt_create_timeout(
                host.as_ptr(),
                RPCPROG_RPCB,
                RPCBVERS4,
                c_nettype.as_ptr(),
                &timeout_short,
            );
            if client.is_null() {
                anyhow::bail!("Error creating RPC client");
            }

            let mut head: *mut Rpcblist = ptr::null_mut();
            let timeout_long = timeval {
                tv_sec: 60,
                tv_usec: 0,
            };

            if client.is_null() {
                anyhow::bail!("client handle is null");
            }

            let call_fn = {
                let ops = (*client).cl_ops;
                if ops.is_null() {
                    anyhow::bail!("client ops pointer is null");
                }
                (*ops).cl_call
            };

            let stat = (call_fn)(
                client,
                RPCBPROC_DUMP,
                xdr_void,
                ptr::null_mut(),
                xdr_rpcblist_ptr,
                &mut head as *mut _ as *mut libc::c_void,
                timeout_long,
            );

            handle_rpc_error(RPCPROG_RPCB, RPCBVERS4, nettype, stat.into())?;

            collect_rpcblist_entries(head)
        }
    }

    /// List registered RPC services via libtirpc's rpcb_getmaps.
    #[cfg(not(target_os = "macos"))]
    pub fn list() -> anyhow::Result<Vec<Entry>> {
        unsafe {
            // "tcp" is enough for the lookup; rpcb_getmaps returns the full
            // service map (all transports) regardless of which one we query on.
            let c_netid = CString::new("tcp").unwrap();
            let nc = getnetconfigent(c_netid.as_ptr());
            if nc.is_null() {
                anyhow::bail!("getnetconfigent(tcp) returned null");
            }
            let c_host = CString::new("127.0.0.1").unwrap();
            let head = rpcb_getmaps(nc, c_host.as_ptr());
            freenetconfigent(nc);
            collect_rpcblist_entries(head)
        }
    }

    unsafe fn collect_rpcblist_entries(head: *mut Rpcblist) -> anyhow::Result<Vec<Entry>> {
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
    enum RpcStatus {
        Success,
        Failure(Option<String>),
    }

    #[cfg(target_os = "macos")]
    impl From<c_int> for RpcStatus {
        fn from(stat: c_int) -> Self {
            match stat {
                RPC_SUCCESS => RpcStatus::Success,
                _ => {
                    let serr = unsafe { clnt_sperrno(stat) };
                    let msg = if !serr.is_null() {
                        Some(unsafe { CStr::from_ptr(serr).to_string_lossy().into_owned() })
                    } else {
                        None
                    };
                    RpcStatus::Failure(msg)
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    impl From<bool> for RpcStatus {
        fn from(success: bool) -> Self {
            if success {
                RpcStatus::Success
            } else {
                RpcStatus::Failure(None)
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    impl From<c_int> for RpcStatus {
        fn from(stat: c_int) -> Self {
            // libtirpc rpcb_set / rpcb_unset return bool_t (typedef int);
            // nonzero means success.
            if stat != 0 {
                RpcStatus::Success
            } else {
                RpcStatus::Failure(None)
            }
        }
    }

    fn handle_rpc_error(
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
