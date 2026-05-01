use libc::{c_char, c_int, c_uint, c_void};
use std::ffi::CString;
use std::ptr;

use super::services::{RpcStatus, collect_rpcblist_entries, handle_rpc_error};
use super::{Entry, Rpcblist, Rpcent};

// Linux uses libtirpc. Its rpcb_* API differs from macOS's oncrpc:
//   - takes `struct netconfig*` (from getnetconfigent("tcp")) instead of a netid string
//   - takes `struct netbuf*` (wraps a sockaddr buffer) instead of `struct sockaddr*`
#[repr(C)]
struct Netbuf {
    maxlen: c_uint,
    len: c_uint,
    buf: *mut c_void,
}

#[repr(C)]
struct Netconfig {
    // We only ever pass the pointer through; field layout is libtirpc-opaque
    // for our purposes. Declaring an empty struct would make Rust treat it as
    // ZST; this at least keeps the type distinct.
    _opaque: [u8; 0],
}

#[link(name = "tirpc")]
unsafe extern "C" {
    fn rpcb_set(
        program: c_uint,
        version: c_uint,
        netconf: *const Netconfig,
        addr: *const Netbuf,
    ) -> c_int;

    fn rpcb_unset(program: c_uint, version: c_uint, netconf: *const Netconfig) -> c_int;

    fn rpcb_getmaps(netconf: *const Netconfig, host: *const c_char) -> *mut Rpcblist;

    fn getnetconfigent(netid: *const c_char) -> *mut Netconfig;
    fn freenetconfigent(netconf: *mut Netconfig);

    // libtirpc: bool_t xdr_rpcblist_ptr(XDR *, rpcblist_ptr *)
    fn xdr_rpcblist_ptr(xdrs: *mut c_void, objp: *mut c_void) -> c_int;
    // libtirpc: void xdr_free(xdrproc_t, char *)
    fn xdr_free(proc: *const c_void, objp: *mut c_char);

    // Shared libc helpers also available on Linux
    pub(super) fn getrpcbynumber(number: c_int) -> *mut Rpcent;
}

pub fn rpcb_set_entry(entry: &Entry) -> anyhow::Result<()> {
    let c_netid = CString::new(entry.netid.as_bytes()).unwrap();
    let nc = unsafe { getnetconfigent(c_netid.as_ptr()) };
    if nc.is_null() {
        anyhow::bail!("getnetconfigent returned null for netid {:?}", entry.netid);
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

pub fn unregister() {
    use super::{RPCPROG_MNT, RPCPROG_NFS, RPCPROG_STAT};
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

/// List registered RPC services via libtirpc's rpcb_getmaps.
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
        let mut head = rpcb_getmaps(nc, c_host.as_ptr());
        freenetconfigent(nc);
        let entries = collect_rpcblist_entries(head);
        // Free the linked list allocated by libtirpc.
        if !head.is_null() {
            xdr_free(
                xdr_rpcblist_ptr as *const c_void,
                &mut head as *mut _ as *mut c_char,
            );
        }
        entries
    }
}

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
