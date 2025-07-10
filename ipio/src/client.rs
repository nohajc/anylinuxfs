#![allow(unused)]

use std::{
    cmp::{max, min},
    error::Error,
    mem::MaybeUninit,
    slice,
    sync::{LazyLock, Mutex, OnceLock, mpsc},
    thread::JoinHandle,
};

use anyhow::Context;

use libc::{c_int, off_t, ssize_t};

use crate::{IORequest, IOResponse};

pub static CLIENT: OnceLock<Mutex<crate::Client>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SvcHandle(libc::c_int);

impl SvcHandle {
    pub fn raw(&self) -> libc::c_int {
        self.0
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CIOVecs {
    iov: *const libc::iovec,
    iovcnt: c_int,
}

unsafe impl Send for CIOVecs {}
unsafe impl Sync for CIOVecs {}

type RequestType = (IORequest, Option<CIOVecs>, mpsc::Sender<IOResponse>);
type RequestSenderType = mpsc::Sender<RequestType>;

static THREADS: LazyLock<Mutex<Vec<JoinHandle<()>>>> = LazyLock::new(|| {
    unsafe { libc::atexit(thread_cleanup) };
    Mutex::new(Vec::new())
});

extern "C" fn thread_cleanup() {
    println!("Cleaning up threads...");

    let mut threads = THREADS.lock().unwrap();
    for thnd in threads.drain(..) {
        if let Err(e) = thnd.join() {
            eprintln!("Failed to join thread: {:?}", e);
        }
    }
}

#[derive(Debug)]
struct ErrnoError(libc::c_int);

impl ErrnoError {
    fn value(&self) -> libc::c_int {
        self.0
    }
}

impl std::fmt::Display for ErrnoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IO error: {}", self.0)
    }
}

impl Error for ErrnoError {}

pub unsafe extern "C" fn preadv(
    hnd: c_int,
    iov: *const libc::iovec,
    iovcnt: c_int,
    offset: off_t,
) -> ssize_t {
    // println!(
    //     "preadv called with handle: {}, iov: {:?}, iovcnt: {}, offset: {}",
    //     hnd, iov, iovcnt, offset
    // );
    let iovecs = unsafe { slice::from_raw_parts(iov, iovcnt as usize) };
    let total_buf_len = iovecs.iter().map(|iov| iov.iov_len as usize).sum();

    let res = preadv_impl(hnd, iovecs, total_buf_len, offset);

    match res {
        Ok(size) => return size,
        Err(e) => {
            if let Some(errno) = e.downcast_ref::<ErrnoError>() {
                unsafe {
                    *libc::__error() = errno.value();
                }
            } else {
                // eprintln!("Error in preadv: {:#}", e);
                unsafe {
                    *libc::__error() = libc::EINVAL;
                }
            }
            return -1;
        }
    }
}

fn preadv_impl(
    _hnd: c_int,
    iovecs: &[libc::iovec],
    total_buf_len: usize,
    offset: off_t,
) -> anyhow::Result<ssize_t> {
    let mut client = CLIENT
        .get()
        .context("IPC Client not initialized")?
        .lock()
        .unwrap();

    let mut shm_size = None;
    if total_buf_len > client.shm.size() as usize {
        let current_size = client.shm.size();
        let new_size = max(current_size * 2, total_buf_len as off_t);
        client.shm.resize(new_size, true)?;
        shm_size = Some(client.shm.size());
    }

    let req = IORequest::Read {
        size: total_buf_len,
        offset,
        shm_size,
    };
    // println!("Sending read request: {:?}", req);
    client.send_request(req)?;
    // println!("Waiting for response...");
    let resp = client.recv_response()?;
    // println!("Received response: {:?}", resp);

    match resp {
        IOResponse::Read { size } => {
            if size < 0 {
                return Err(anyhow::anyhow!("Unexpected read error"));
            }
            let resp_data = unsafe { client.shm.data() };
            let mut buf_pos = 0;
            let mut remaining_size = size as usize;
            for iov in iovecs {
                let buf = unsafe {
                    slice::from_raw_parts_mut(
                        iov.iov_base as *mut MaybeUninit<u8>,
                        iov.iov_len as usize,
                    )
                };
                let buf_size = min(iov.iov_len as usize, remaining_size);
                buf.copy_from_slice(&resp_data[buf_pos..buf_pos + buf_size]);
                buf_pos += iov.iov_len as usize;
                remaining_size -= iov.iov_len as usize;
            }
            Ok(size)
        }
        IOResponse::Error { errno } => Err(ErrnoError(errno).into()),
        _ => Err(anyhow::anyhow!("Unexpected response header: {:?}", resp)),
    }
}

pub unsafe extern "C" fn pwritev(
    hnd: c_int,
    iov: *const libc::iovec,
    iovcnt: c_int,
    offset: off_t,
) -> ssize_t {
    let iovecs = unsafe { slice::from_raw_parts(iov, iovcnt as usize) };
    let total_buf_len = iovecs.iter().map(|iov| iov.iov_len as usize).sum();

    let res = pwritev_impl(hnd, iovecs, total_buf_len, offset);

    match res {
        Ok(size) => return size,
        Err(e) => {
            if let Some(errno) = e.downcast_ref::<ErrnoError>() {
                unsafe {
                    *libc::__error() = errno.value();
                }
            } else {
                // eprintln!("Error in pwritev: {:#}", e);
                unsafe {
                    *libc::__error() = libc::EINVAL;
                }
            }
            return -1;
        }
    }
}

fn pwritev_impl(
    _hnd: c_int,
    iovecs: &[libc::iovec],
    total_buf_len: usize,
    offset: off_t,
) -> anyhow::Result<ssize_t> {
    let mut client = CLIENT
        .get()
        .context("IPC Client not initialized")?
        .lock()
        .unwrap();

    let mut shm_size = None;
    if total_buf_len > client.shm.size() as usize {
        let current_size = client.shm.size();
        let new_size = max(current_size * 2, total_buf_len as off_t);
        client.shm.resize(new_size, true)?;
        shm_size = Some(client.shm.size());
    }

    let req = IORequest::Write {
        size: total_buf_len,
        offset,
        shm_size,
    };

    let req_data = unsafe { client.shm.data() };
    let mut buf_pos = 0;
    for iov in iovecs {
        let buf = unsafe {
            slice::from_raw_parts(iov.iov_base as *const MaybeUninit<u8>, iov.iov_len as usize)
        };
        req_data[buf_pos..buf_pos + iov.iov_len as usize].copy_from_slice(buf);
        buf_pos += iov.iov_len as usize;
    }

    // println!("Sending write request: {:?}", req);
    client.send_request(req)?;
    // println!("Waiting for response...");
    let resp = client.recv_response()?;
    // println!("Received response: {:?}", resp);

    match resp {
        IOResponse::Write { size } => {
            if size < 0 {
                return Err(anyhow::anyhow!("Unexpected write error"));
            }
            Ok(size)
        }
        IOResponse::Error { errno } => Err(ErrnoError(errno).into()),
        _ => Err(anyhow::anyhow!("Unexpected response header: {:?}", resp)),
    }
}

pub unsafe extern "C" fn size(hnd: c_int) -> ssize_t {
    let res = size_impl(hnd);

    match res {
        Ok(size) => return size,
        Err(e) => {
            if let Some(errno) = e.downcast_ref::<ErrnoError>() {
                unsafe {
                    *libc::__error() = errno.value();
                }
            } else {
                // eprintln!("Error in size: {:#}", e);
                unsafe {
                    *libc::__error() = libc::EINVAL;
                }
            }
            return -1;
        }
    }
}

fn size_impl(_hnd: c_int) -> anyhow::Result<ssize_t> {
    let mut client = CLIENT
        .get()
        .context("IPC Client not initialized")?
        .lock()
        .unwrap();

    client.send_request(IORequest::Size)?;
    let resp = client.recv_response()?;

    match resp {
        IOResponse::Size { size } => {
            if size < 0 {
                return Err(anyhow::anyhow!("Unexpected size error"));
            }
            Ok(size)
        }
        IOResponse::Error { errno } => Err(ErrnoError(errno).into()),
        _ => Err(anyhow::anyhow!("Unexpected response header: {:?}", resp)),
    }
}
