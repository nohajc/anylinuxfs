use libc::{c_char, c_int, c_uint, c_void, sockaddr, timeval};
use std::ffi::{CStr, CString};
use std::ptr;

use super::services::{RpcStatus, collect_rpcblist_entries, handle_rpc_error};
use super::{Entry, Rpcblist, Rpcent};

#[link(name = "oncrpc", kind = "framework")]
unsafe extern "C" {
    /// int rpcb_unset(const char *netid, unsigned int program, unsigned int version);
    #[link_name = "_newrpclib_rpcb_unset"]
    fn rpcb_unset_mac(netid: *const c_char, program: c_uint, version: c_uint) -> c_int;

    /// int rpcb_set(const char *netid, unsigned int program, unsigned int version,
    ///              const struct sockaddr *addr);
    #[link_name = "_newrpclib_rpcb_set"]
    fn rpcb_set_mac(
        netid: *const c_char,
        program: c_uint,
        version: c_uint,
        addr: *const sockaddr,
    ) -> bool;

    /* Additional declarations pulled from <rpc/rpc.h> and related headers */
    #[link_name = "_newrpclib_clnt_create_timeout"]
    fn clnt_create_timeout(
        host: *const c_char,
        prognum: c_uint,
        versnum: c_uint,
        nettype: *const c_char,
        timeout: *const timeval,
    ) -> *mut CLIENT;

    #[link_name = "_newrpclib_xdr_rpcblist_ptr"]
    fn xdr_rpcblist_ptr(xdrs: *mut c_void, objp: *mut c_void, len: c_uint) -> c_int;

    fn xdr_void() -> c_int;
    pub(super) fn clnt_sperrno(stat: c_int) -> *const c_char;
    pub(super) fn getrpcbynumber(number: c_int) -> *mut Rpcent;
}

#[allow(non_camel_case_types)]
type xdrproc_t = unsafe extern "C" fn(xdrs: *mut c_void, addrp: *mut c_void, len: c_uint) -> c_int;

#[allow(non_camel_case_types)]
type xdrproc_void_t = unsafe extern "C" fn() -> c_int;

#[repr(C)]
struct ClntOps {
    cl_call: extern "C" fn(
        *mut CLIENT,
        c_uint,
        xdrproc_void_t,
        *mut c_void,
        xdrproc_t,
        *mut c_void,
        timeval,
    ) -> c_int,

    cl_abort: extern "C" fn(),
    cl_geterr: extern "C" fn(*mut CLIENT, *mut c_void),
    cl_freeres: extern "C" fn(*mut CLIENT, *const c_void, *mut c_void) -> c_int,
    cl_destroy: extern "C" fn(*mut CLIENT),
    cl_control: extern "C" fn(*mut CLIENT, c_int, *mut c_char) -> c_int,
}

#[repr(C)]
struct CLIENT {
    cl_auth: *mut c_void,
    cl_ops: *mut ClntOps,
    cl_private: *mut c_void,
}

const RPCPROG_RPCB: c_uint = 100000;
const RPCBVERS4: c_uint = 4;
const RPCBPROC_DUMP: c_uint = 4;
pub(super) const RPC_SUCCESS: c_int = 0;

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

pub fn unregister() {
    use super::{RPCPROG_MNT, RPCPROG_NFS, RPCPROG_STAT};
    unsafe {
        rpcb_unset_mac(ptr::null(), RPCPROG_NFS, 3);
        rpcb_unset_mac(ptr::null(), RPCPROG_NFS, 4);
        rpcb_unset_mac(ptr::null(), RPCPROG_MNT, 1);
        rpcb_unset_mac(ptr::null(), RPCPROG_MNT, 2);
        rpcb_unset_mac(ptr::null(), RPCPROG_MNT, 3);
        rpcb_unset_mac(ptr::null(), RPCPROG_STAT, 1);
    }
}

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

        let entries = collect_rpcblist_entries(head);
        // Free the linked list allocated by xdr_rpcblist_ptr during decode.
        // CLNT_FREERES re-invokes the XDR proc with op=XDR_FREE, which
        // walks the list and releases each node and its strings.
        if !head.is_null() {
            let cl_freeres = (*(*client).cl_ops).cl_freeres;
            (cl_freeres)(
                client,
                xdr_rpcblist_ptr as *const c_void,
                &mut head as *mut _ as *mut c_void,
            );
        }
        entries
    }
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
