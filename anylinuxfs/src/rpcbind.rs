use libc::{c_char, c_int, c_uint, c_void, sockaddr, timeval};
use os_socketaddr::OsSocketAddr;
use std::ffi::CStr;
use std::net::SocketAddr;
use std::{ffi::CString, ptr};

use anyhow::anyhow;

#[link(name = "oncrpc", kind = "framework")]
unsafe extern "C" {
    /// int rpcb_unset(const char *netid, unsigned int program, unsigned int version);
    #[link_name = "_newrpclib_rpcb_unset"]
    pub fn rpcb_unset(netid: *const c_char, program: c_uint, version: c_uint) -> c_int;

    /// int rpcb_set(const char *netid, unsigned int program, unsigned int version,
    ///              const struct sockaddr *addr);
    #[link_name = "_newrpclib_rpcb_set"]
    pub fn rpcb_set(
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

#[allow(non_camel_case_types)]
pub type xdrproc_t =
    unsafe extern "C" fn(xdrs: *mut c_void, addrp: *mut c_void, len: c_uint) -> c_int;

#[allow(non_camel_case_types)]
pub type xdrproc_void_t = unsafe extern "C" fn() -> c_int;

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

    const RPCBVERS4: c_uint = 4;
    const RPCBPROC_DUMP: c_uint = 4;
    const RPC_SUCCESS: c_int = 0;

    pub fn rpcb_set_entry(entry: &Entry) -> anyhow::Result<()> {
        let c_netid = CString::new(entry.netid.as_bytes()).unwrap();
        let stat = unsafe {
            rpcb_set(
                c_netid.as_ptr(),
                entry.prog,
                entry.vers,
                entry.addr.as_ptr(),
            )
        };
        Ok(handle_rpc_error(
            entry.prog,
            entry.vers,
            &entry.netid,
            stat.into(),
        )?)
    }

    pub fn rpcb_set_entries(entries: &[Entry]) -> anyhow::Result<()> {
        for entry in entries {
            rpcb_set_entry(entry)?;
        }
        Ok(())
    }

    /// Unregister the NFS, MOUNT and STAT program/version pairs.
    pub fn unregister() {
        unsafe {
            rpcb_unset(ptr::null(), RPCPROG_NFS, 3);
            rpcb_unset(ptr::null(), RPCPROG_NFS, 4);
            rpcb_unset(ptr::null(), RPCPROG_MNT, 1);
            rpcb_unset(ptr::null(), RPCPROG_MNT, 2);
            rpcb_unset(ptr::null(), RPCPROG_MNT, 3);
            rpcb_unset(ptr::null(), RPCPROG_STAT, 1);
        }
    }

    /// Register NFS, MOUNT and STAT services.
    pub fn register() -> anyhow::Result<()> {
        let ip_props = [("", IpAddr::from([0; 4])), ("6", IpAddr::from([0; 16]))];
        let progs = [
            (RPCPROG_NFS, NFS_PORT, vec![3, 4]),
            (RPCPROG_MNT, MOUNT_PORT, vec![1, 2, 3]),
            (RPCPROG_STAT, STAT_PORT, vec![1]),
        ];

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
                        rpcb_set_entry(&e)?;
                    }
                }
            }
        }

        Ok(())
    }

    /// List registered RPC services by querying rpcbind.
    pub fn list() -> anyhow::Result<Vec<Entry>> {
        unsafe {
            let host = CString::new("localhost").unwrap();
            let nettype = "udp";
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
                return Err(anyhow!("Error creating RPC client"));
            }

            let mut head: *mut Rpcblist = ptr::null_mut();
            let timeout_long = timeval {
                tv_sec: 60,
                tv_usec: 0,
            };

            if client.is_null() {
                return Err(anyhow!("client handle is null"));
            }

            let call_fn = {
                let ops = (*client).cl_ops;
                if ops.is_null() {
                    return Err(anyhow!("client ops pointer is null"));
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

            if head.is_null() {
                return Ok(Vec::new());
            }

            let mut res = Vec::new();

            let mut cur = head;
            while !cur.is_null() {
                let map = &(*cur).rpcb_map;
                let prog = map.r_prog;
                let vers = map.r_vers;

                let netid = if map.r_netid.is_null() {
                    "".to_string()
                } else {
                    CStr::from_ptr(map.r_netid).to_string_lossy().into_owned()
                };
                let addr_string = if map.r_addr.is_null() {
                    "".to_string()
                } else {
                    CStr::from_ptr(map.r_addr).to_string_lossy().into_owned()
                };
                let owner = if map.r_owner.is_null() {
                    "".to_string()
                } else {
                    CStr::from_ptr(map.r_owner).to_string_lossy().into_owned()
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

                cur = (*cur).rpcb_next;
            }

            Ok(res)
        }
    }

    #[derive(Debug, Clone)]
    enum RpcStatus {
        Success,
        Failure(Option<String>),
    }

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

    impl From<bool> for RpcStatus {
        fn from(success: bool) -> Self {
            if success {
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
                Some(m) => return Err(anyhow!("{} RPC call failed: {}", svc_str, m)),
                None => return Err(anyhow!("{} RPC call failed", svc_str)),
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
