// there's probably no need of server-side synchronization because Linux kernel should already handle that
// we also need to handle splitting or merging the iovec buffers to achieve vectorized I/O

use std::{
    cell::RefCell,
    cmp::min,
    collections::{HashMap, hash_map::Entry},
    error::Error,
    mem::MaybeUninit,
    slice,
    sync::{LazyLock, Mutex, atomic::AtomicI32},
    thread,
};

use anyhow::Context;
use iceoryx2::{
    port::client::Client, prelude::*, service::port_factory::request_response::PortFactory,
};
use libc::{c_int, off_t, ssize_t};

use crate::{IORequest, IOResponse};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SvcHandle(libc::c_int);

static NODE: LazyLock<Node<ipc::Service>> = LazyLock::new(|| {
    NodeBuilder::new()
        .create::<ipc::Service>()
        .expect("Failed to create IPC node")
});

type ServiceType = PortFactory<ipc::Service, [u8], IORequest, [u8], IOResponse>;
type ClientType = Client<ipc::Service, [u8], IORequest, [u8], IOResponse>;

static SVC_HND_COUNTER: AtomicI32 = AtomicI32::new(42);

static SERVICE_NAMES: LazyLock<Mutex<HashMap<SvcHandle, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static SERVICES: LazyLock<Mutex<HashMap<String, ServiceType>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

thread_local! {
    static CLIENTS: LazyLock<RefCell<HashMap<SvcHandle, ClientType>>> =
        LazyLock::new(|| RefCell::new(HashMap::new()));
}

pub fn new_service(service_name: impl AsRef<str>) -> anyhow::Result<SvcHandle> {
    let node = &*NODE;

    let mut services = SERVICES.lock().unwrap();
    if services.contains_key(service_name.as_ref()) {
        return Err(anyhow::anyhow!(
            "Service '{}' already exists",
            service_name.as_ref()
        ));
    }

    services.insert(
        service_name.as_ref().into(),
        node.service_builder(&service_name.as_ref().try_into().unwrap())
            .request_response::<[u8], [u8]>()
            .request_user_header::<IORequest>()
            .response_user_header::<IOResponse>()
            .open_or_create()
            .expect("Failed to create IPC service"),
    );

    let mut service_names = SERVICE_NAMES.lock().unwrap();
    let handle = SvcHandle(SVC_HND_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    service_names.insert(handle, service_name.as_ref().into());

    Ok(handle)
}

// for a given service handle, look up or create
// the client in thread-local storage and pass it to `f`
pub fn with_client<R>(
    handle: SvcHandle,
    f: impl FnOnce(&mut ClientType) -> anyhow::Result<R>,
) -> anyhow::Result<R> {
    CLIENTS.with(|clients| {
        let mut clients = clients.borrow_mut();

        let client = match clients.entry(handle) {
            Entry::Occupied(oe) => oe.into_mut(),
            Entry::Vacant(ve) => {
                let service_names = SERVICE_NAMES.lock().unwrap();
                let service_name = service_names
                    .get(&handle)
                    .ok_or_else(|| anyhow::anyhow!("Service handle {:?} not found", handle))?;

                println!(
                    "Initializing new client for service: {} (thread: {:?})",
                    service_name,
                    thread::current().id()
                );

                let services = SERVICES.lock().unwrap();
                let svc = services
                    .get(service_name)
                    .ok_or_else(|| anyhow::anyhow!("Service '{}' not found", service_name))?;

                let client = svc
                    .client_builder()
                    .initial_max_slice_len(4096)
                    .allocation_strategy(AllocationStrategy::PowerOfTwo)
                    .create()
                    .context("Failed to create IPC client")?;

                ve.insert(client)
            }
        };

        f(client)
    })
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

pub unsafe fn readv(hnd: c_int, iov: *const libc::iovec, iovcnt: c_int, offset: off_t) -> ssize_t {
    let iovecs = unsafe { slice::from_raw_parts(iov, iovcnt as usize) };
    let total_buf_len = iovecs.iter().map(|iov| iov.iov_len as usize).sum();

    let res = with_client(SvcHandle(hnd), |client| -> anyhow::Result<ssize_t> {
        let mut req_data = client.loan_slice_uninit(0)?;
        let req = req_data.user_header_mut();
        *req = IORequest::Read {
            size: total_buf_len,
            offset,
        };
        let req = unsafe { req_data.assume_init() };
        let pending_resp = req.send()?;

        if let Some(resp) = pending_resp.receive()? {
            let resp_data = resp.payload();
            let resp_header = resp.user_header();
            match *resp_header {
                IOResponse::Read { size } => {
                    if size < 0 {
                        return Err(anyhow::anyhow!("Unexpecteed read error"));
                    }

                    let mut buf_pos = 0;
                    let mut remaining_size = size as usize;
                    for iov in iovecs {
                        let buf = unsafe {
                            slice::from_raw_parts_mut(iov.iov_base as *mut u8, iov.iov_len as usize)
                        };
                        let buf_size = min(buf_pos + iov.iov_len as usize, remaining_size);
                        buf.copy_from_slice(&resp_data[buf_pos..buf_size]);
                        buf_pos += iov.iov_len as usize;
                        remaining_size -= iov.iov_len as usize;
                    }

                    return Ok(size);
                }
                IOResponse::Error { errno } => {
                    return Err(ErrnoError(errno).into());
                }
                _ => {
                    return Err(anyhow::anyhow!(
                        "Unexpected response header: {:?}",
                        resp_header
                    ));
                }
            }
        }

        return Err(anyhow::anyhow!("No response received"));
    });

    match res {
        Ok(size) => return size,
        Err(e) => {
            if let Some(errno) = e.downcast_ref::<ErrnoError>() {
                unsafe {
                    *libc::__error() = errno.value();
                }
            } else {
                eprintln!("Error in readv: {:#}", e);
                unsafe {
                    *libc::__error() = libc::EINVAL;
                }
            }
            return -1;
        }
    }
}

pub unsafe fn writev(hnd: c_int, iov: *const libc::iovec, iovcnt: c_int, offset: off_t) -> ssize_t {
    let iovecs = unsafe { slice::from_raw_parts(iov, iovcnt as usize) };
    let total_buf_len: usize = iovecs.iter().map(|iov| iov.iov_len as usize).sum();

    let res = with_client(SvcHandle(hnd), |client| -> anyhow::Result<ssize_t> {
        let mut req = client.loan_slice_uninit(total_buf_len)?;
        let req_header = req.user_header_mut();
        *req_header = IORequest::Write { offset };
        let req_data = req.payload_mut();

        let mut buf_pos = 0;
        for iov in iovecs {
            // iovec buffer contents should be initialized by the caller
            // we just use MaybeUninit type so it is compatible with req_data
            let buf = unsafe {
                slice::from_raw_parts(iov.iov_base as *const MaybeUninit<u8>, iov.iov_len as usize)
            };
            req_data[buf_pos..buf_pos + iov.iov_len as usize].copy_from_slice(buf);
            buf_pos += iov.iov_len as usize;
        }

        let req = unsafe { req.assume_init() };
        let pending_resp = req.send()?;

        if let Some(resp) = pending_resp.receive()? {
            let resp_header = resp.user_header();
            match *resp_header {
                IOResponse::Write { size } => {
                    if size < 0 {
                        return Err(anyhow::anyhow!("Unexpected write error"));
                    }
                    return Ok(size);
                }
                IOResponse::Error { errno } => {
                    return Err(ErrnoError(errno).into());
                }
                _ => {
                    return Err(anyhow::anyhow!(
                        "Unexpected response header: {:?}",
                        resp_header
                    ));
                }
            }
        }

        return Err(anyhow::anyhow!("No response received"));
    });

    match res {
        Ok(size) => return size,
        Err(e) => {
            if let Some(errno) = e.downcast_ref::<ErrnoError>() {
                unsafe {
                    *libc::__error() = errno.value();
                }
            } else {
                eprintln!("Error in writev: {:#}", e);
                unsafe {
                    *libc::__error() = libc::EINVAL;
                }
            }
            return -1;
        }
    }
}

pub unsafe fn size(hnd: c_int) -> ssize_t {
    let res = with_client(SvcHandle(hnd), |client| -> anyhow::Result<ssize_t> {
        let mut req_data = client.loan_slice_uninit(0)?;
        let req = req_data.user_header_mut();
        *req = IORequest::Size;
        let req = unsafe { req_data.assume_init() };
        let pending_resp = req.send()?;

        if let Some(resp) = pending_resp.receive()? {
            let resp_header = resp.user_header();
            match *resp_header {
                IOResponse::Size { size } => return Ok(size),
                IOResponse::Error { errno } => return Err(ErrnoError(errno).into()),
                _ => {
                    return Err(anyhow::anyhow!(
                        "Unexpected response header: {:?}",
                        resp_header
                    ));
                }
            }
        }

        return Err(anyhow::anyhow!("No response received"));
    });

    match res {
        Ok(size) => return size,
        Err(e) => {
            if let Some(errno) = e.downcast_ref::<ErrnoError>() {
                unsafe {
                    *libc::__error() = errno.value();
                }
            } else {
                eprintln!("Error in size: {:#}", e);
                unsafe {
                    *libc::__error() = libc::EINVAL;
                }
            }
            return -1;
        }
    }
}
