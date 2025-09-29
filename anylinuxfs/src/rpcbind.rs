#![allow(unused)]
use libc::timeval;
use libc::{
    AF_INET, AF_INET6, INADDR_ANY, c_char, c_int, c_uint, htons, in_addr, in6_addr, sa_family_t,
    sockaddr, sockaddr_in, sockaddr_in6, sockaddr_storage,
};
use os_socketaddr::OsSocketAddr;
use std::ffi::CStr;
use std::net::SocketAddr;
use std::{ffi::CString, mem, ptr};

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
    ) -> c_int;

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
    pub fn xdr_rpcblist_ptr(xdrs: *mut libc::c_void, objp: *mut libc::c_void, len: c_uint)
    -> c_int;

    pub fn xdr_void() -> c_int;
    pub fn clnt_sperrno(stat: c_int) -> *const c_char;
    // pub fn getrpcbynumber(number: c_int) -> *mut Rpcent;
}

#[allow(non_camel_case_types)]
pub type xdrproc_t =
    unsafe extern "C" fn(xdrs: *mut libc::c_void, addrp: *mut libc::c_void, len: c_uint) -> c_int;

#[allow(non_camel_case_types)]
pub type xdrproc_void_t = unsafe extern "C" fn() -> c_int;

#[repr(C)]
pub struct ClntOps {
    pub cl_call: extern "C" fn(
        *mut CLIENT,
        c_uint,
        xdrproc_void_t,
        *mut libc::c_void,
        xdrproc_t,
        *mut libc::c_void,
        timeval,
    ) -> c_int,

    pub cl_abort: extern "C" fn(),
    pub cl_geterr: extern "C" fn(*mut CLIENT, *mut libc::c_void),
    pub cl_freeres: extern "C" fn(*mut CLIENT, *const libc::c_void, *mut libc::c_void) -> c_int,
    pub cl_destroy: extern "C" fn(*mut CLIENT),
    pub cl_control: extern "C" fn(*mut CLIENT, c_int, *mut c_char) -> c_int,
}

#[repr(C)]
pub struct CLIENT {
    pub cl_auth: *mut libc::c_void,
    pub cl_ops: *mut ClntOps,
    pub cl_private: *mut libc::c_void,
}

// #[repr(C)]
// pub struct Rpcent {
//     pub r_name: *mut c_char,
//     pub r_aliases: *mut *mut c_char,
//     pub r_number: c_int,
// }

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

    const NFSUDPPORT: u16 = 2049;
    const NFSUDP6PORT: u16 = NFSUDPPORT;
    const NFSTCPPORT: u16 = NFSUDPPORT;
    const NFSTCP6PORT: u16 = NFSTCPPORT;

    const MOUNTUDPPORT: u16 = 32767;
    const MOUNTUDP6PORT: u16 = MOUNTUDPPORT;
    const MOUNTTCPPORT: u16 = MOUNTUDPPORT;
    const MOUNTTCP6PORT: u16 = MOUNTTCPPORT;

    const STATDUDPPPORT: u16 = 32765;
    const STATDUDP6PORT: u16 = STATDUDPPPORT;
    const STATDTCPPORT: u16 = STATDUDPPPORT;
    const STATDTCP6PORT: u16 = STATDTCPPORT;

    const RPCBVERS4: c_uint = 4;
    const RPCBPROC_DUMP: c_uint = 4;
    const RPC_SUCCESS: c_int = 0;

    fn rpcb_set_vers(
        netid: *const c_char,
        program: c_uint,
        versions: &[c_uint],
        addr: *const sockaddr,
    ) -> c_int {
        for &ver in versions {
            if unsafe { rpcb_set(netid, program, ver, addr) } == 0 {
                return 0;
            }
        }
        1
    }

    pub fn rpcb_set_entry(entry: &Entry) -> c_int {
        let c_netid = CString::new(entry.netid.as_bytes()).unwrap();
        unsafe {
            rpcb_set(
                c_netid.as_ptr(),
                entry.prog,
                entry.vers,
                entry.addr.as_ptr(),
            )
        }
    }

    pub fn rpcb_set_entries(entries: &[Entry]) -> c_int {
        let mut result = 1;
        for entry in entries {
            let res = rpcb_set_entry(entry);
            if res == 0 {
                result = 0;
            }
        }
        result
    }

    /// Unregister the NFS and MOUNT program/version pairs.
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

    /// Register NFS and MOUNT services.
    pub fn register() -> anyhow::Result<()> {
        let mut errors: Vec<String> = Vec::new();

        // Prepare sockaddr_storage containers for IPv4 and IPv6
        let mut ss: sockaddr_storage = unsafe { mem::zeroed() };
        let mut ss6: sockaddr_storage = unsafe { mem::zeroed() };

        // Populate IPv4 and IPv6 sockaddr structures inside the storages
        unsafe {
            let sin: *mut sockaddr_in = &mut ss as *mut _ as *mut sockaddr_in;
            ptr::write_bytes(sin as *mut u8, 0, mem::size_of::<sockaddr_in>());
            (*sin).sin_family = AF_INET as sa_family_t;
            (*sin).sin_port = htons(NFSUDPPORT);
            (*sin).sin_addr = in_addr { s_addr: INADDR_ANY };
            // BSD/macOS have sin_len
            #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
            {
                (*sin).sin_len = mem::size_of::<sockaddr_in>() as u8;
            }

            let sin6: *mut sockaddr_in6 = &mut ss6 as *mut _ as *mut sockaddr_in6;
            ptr::write_bytes(sin6 as *mut u8, 0, mem::size_of::<sockaddr_in6>());
            (*sin6).sin6_family = AF_INET6 as sa_family_t;
            (*sin6).sin6_port = htons(NFSUDP6PORT);
            (*sin6).sin6_addr = in6_addr { s6_addr: [0; 16] };
            #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
            {
                (*sin6).sin6_len = mem::size_of::<sockaddr_in6>() as u8;
            }
        }

        // Prepare CStrings for netids
        let c_udp = CString::new("udp").unwrap();
        let c_tcp = CString::new("tcp").unwrap();
        let c_udp6 = CString::new("udp6").unwrap();
        let c_tcp6 = CString::new("tcp6").unwrap();

        // --- Register NFS ---
        unsafe {
            // NFS UDP
            let sin: *mut sockaddr_in = &mut ss as *mut _ as *mut sockaddr_in;
            (*sin).sin_port = htons(NFSUDPPORT);
            if rpcb_set(
                c_udp.as_ptr(),
                RPCPROG_NFS,
                3,
                &ss as *const _ as *const sockaddr,
            ) == 0
            {
                errors.push("couldn't register NFS/UDP".into());
            }

            // NFS UDP6
            let sin6: *mut sockaddr_in6 = &mut ss6 as *mut _ as *mut sockaddr_in6;
            (*sin6).sin6_port = htons(NFSUDP6PORT);
            if rpcb_set(
                c_udp6.as_ptr(),
                RPCPROG_NFS,
                3,
                &ss6 as *const _ as *const sockaddr,
            ) == 0
            {
                errors.push("couldn't register NFS/UDP6".into());
            }
        }

        // NFS TCP
        unsafe {
            let sin: *mut sockaddr_in = &mut ss as *mut _ as *mut sockaddr_in;
            (*sin).sin_port = htons(NFSTCPPORT);
            if rpcb_set_vers(
                c_tcp.as_ptr(),
                RPCPROG_NFS,
                &[3, 4],
                &ss as *const _ as *const sockaddr,
            ) == 0
            {
                errors.push("couldn't register NFS/TCP".into());
            }

            let sin6: *mut sockaddr_in6 = &mut ss6 as *mut _ as *mut sockaddr_in6;
            (*sin6).sin6_port = htons(NFSTCP6PORT);
            if rpcb_set_vers(
                c_tcp6.as_ptr(),
                RPCPROG_NFS,
                &[3, 4],
                &ss6 as *const _ as *const sockaddr,
            ) == 0
            {
                errors.push("couldn't register NFS/TCP6".into());
            }
        }

        // --- Register MOUNTD ---
        unsafe {
            let sin: *mut sockaddr_in = &mut ss as *mut _ as *mut sockaddr_in;
            (*sin).sin_port = htons(MOUNTUDPPORT);
            if rpcb_set_vers(
                c_udp.as_ptr(),
                RPCPROG_MNT,
                &[1, 2, 3],
                &ss as *const _ as *const sockaddr,
            ) == 0
            {
                errors.push("couldn't register MOUNT/UDP".into());
            }

            let sin6: *mut sockaddr_in6 = &mut ss6 as *mut _ as *mut sockaddr_in6;
            (*sin6).sin6_port = htons(MOUNTUDP6PORT);
            if rpcb_set_vers(
                c_udp6.as_ptr(),
                RPCPROG_MNT,
                &[1, 2, 3],
                &ss6 as *const _ as *const sockaddr,
            ) == 0
            {
                errors.push("couldn't register MOUNT/UDP6".into());
            }

            (*sin).sin_port = htons(MOUNTTCPPORT);
            if rpcb_set_vers(
                c_tcp.as_ptr(),
                RPCPROG_MNT,
                &[1, 2, 3],
                &ss as *const _ as *const sockaddr,
            ) == 0
            {
                errors.push("couldn't register MOUNT/TCP".into());
            }

            (*sin6).sin6_port = htons(MOUNTTCP6PORT);
            if rpcb_set_vers(
                c_tcp6.as_ptr(),
                RPCPROG_MNT,
                &[1, 2, 3],
                &ss6 as *const _ as *const sockaddr,
            ) == 0
            {
                errors.push("couldn't register MOUNT/TCP6".into());
            }
        }

        // --- Register STATD ---
        unsafe {
            let sin: *mut sockaddr_in = &mut ss as *mut _ as *mut sockaddr_in;
            (*sin).sin_port = htons(STATDUDPPPORT);
            if rpcb_set(
                c_udp.as_ptr(),
                RPCPROG_STAT,
                1,
                &ss as *const _ as *const sockaddr,
            ) == 0
            {
                errors.push("couldn't register STATD/UDP".into());
            }

            let sin6: *mut sockaddr_in6 = &mut ss6 as *mut _ as *mut sockaddr_in6;
            (*sin6).sin6_port = htons(STATDUDP6PORT);
            if rpcb_set(
                c_udp6.as_ptr(),
                RPCPROG_STAT,
                1,
                &ss6 as *const _ as *const sockaddr,
            ) == 0
            {
                errors.push("couldn't register STATD/UDP6".into());
            }

            (*sin).sin_port = htons(STATDTCPPORT);
            if rpcb_set(
                c_tcp.as_ptr(),
                RPCPROG_STAT,
                1,
                &ss as *const _ as *const sockaddr,
            ) == 0
            {
                errors.push("couldn't register STATD/TCP".into());
            }

            (*sin6).sin6_port = htons(STATDTCP6PORT);
            if rpcb_set(
                c_tcp6.as_ptr(),
                RPCPROG_STAT,
                1,
                &ss6 as *const _ as *const sockaddr,
            ) == 0
            {
                errors.push("couldn't register STATD/TCP6".into());
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow!("{}", errors.join(", ")))
        }
    }

    /// List registered RPC services by querying rpcbind.
    pub fn list() -> anyhow::Result<Vec<Entry>> {
        unsafe {
            let host = CString::new("localhost").unwrap();
            let nettype = CString::new("udp").unwrap();

            let timeout_short = timeval {
                tv_sec: 5,
                tv_usec: 0,
            };
            let client = clnt_create_timeout(
                host.as_ptr(),
                RPCPROG_RPCB,
                RPCBVERS4,
                nettype.as_ptr(),
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

            if stat != RPC_SUCCESS {
                let serr = clnt_sperrno(stat);
                let msg = if !serr.is_null() {
                    CStr::from_ptr(serr).to_string_lossy().into_owned()
                } else {
                    format!("clnt_call failed with status {}", stat)
                };
                return Err(anyhow!("RPC call failed: {}", msg));
            }

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

                // let rpc = getrpcbynumber(prog as c_int);
                // let svc = if rpc.is_null() || (*rpc).r_name.is_null() {
                //     "-".to_string()
                // } else {
                //     CStr::from_ptr((*rpc).r_name).to_string_lossy().into_owned()
                // };

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
