// there's probably no need of server-side synchronization because Linux kernel should already handle that
// we also need to handle splitting or merging the iovec buffers to achieve vectorized I/O

use std::{
    cmp::min,
    collections::HashMap,
    error::Error,
    mem::{self, MaybeUninit},
    slice,
    sync::{LazyLock, Mutex, atomic::AtomicI32, mpsc},
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::Context;
use iceoryx2::{
    node::{Node, NodeBuilder},
    port::client::Client,
    prelude::{AllocationStrategy, LogLevel, set_log_level},
    service::{ipc, port_factory::request_response::PortFactory},
};
use libc::{c_int, off_t, ssize_t};

use crate::{IORequest, IOResponse};

const CYCLE_TIME: Duration = Duration::from_millis(5); // 5 ms

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SvcHandle(libc::c_int);

impl SvcHandle {
    pub fn raw(&self) -> libc::c_int {
        self.0
    }
}

type ServiceType = PortFactory<ipc::Service, [u8], IORequest, [u8], IOResponse>;
type ClientType = Client<ipc::Service, [u8], IORequest, [u8], IOResponse>;

#[derive(Debug, Clone, Copy)]
pub struct CIOVecs {
    iov: *const libc::iovec,
    iovcnt: c_int,
}

unsafe impl Send for CIOVecs {}
unsafe impl Sync for CIOVecs {}

type RequestType = (IORequest, Option<CIOVecs>, mpsc::Sender<IOResponse>);
type RequestSenderType = mpsc::Sender<RequestType>;

static SVC_HND_COUNTER: AtomicI32 = AtomicI32::new(42);

static SERVICE_NAMES: LazyLock<Mutex<HashMap<SvcHandle, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static SERVICES: LazyLock<Mutex<HashMap<String, RequestSenderType>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static NODE: LazyLock<Mutex<Node<ipc::Service>>> = LazyLock::new(|| {
    Mutex::new(
        NodeBuilder::new()
            .create::<ipc::Service>()
            .expect("Failed to create IPC node"),
    )
});

static THREADS: LazyLock<Mutex<Vec<JoinHandle<()>>>> = LazyLock::new(|| {
    unsafe { libc::atexit(thread_cleanup) };
    Mutex::new(Vec::new())
});

extern "C" fn thread_cleanup() {
    println!("Cleaning up threads...");
    let mut services = SERVICES.lock().unwrap();
    services.clear();

    let mut threads = THREADS.lock().unwrap();
    for thnd in threads.drain(..) {
        if let Err(e) = thnd.join() {
            eprintln!("Failed to join thread: {:?}", e);
        }
    }
}

// thread_local! {
// static mut CLIENTS: LazyLock<Mutex<HashMap<SvcHandle, ClientType>>> =
//     LazyLock::new(|| Mutex::new(HashMap::new()));
// }

// fn with_reconnect<T>(
//     svc: &ServiceType,
//     client: &mut ClientType,
//     f: impl Fn() -> Result<Option<T>, ReceiveError>,
// ) -> Result<Option<T>, ReceiveError> {
//     let mut retries = 0;
//     loop {
//         if retries > 0 {
//             println!("Retrying... Attempt {}", retries + 1);
//         }
//         let res = f();
//         match res.as_ref() {
//             Ok(Some(_)) | Err(_) => return res,
//             Ok(None) => {
//                 if retries < 3 {
//                     retries += 1;
//                     thread::sleep(CYCLE_TIME);
//                 } else {
//                     return res;
//                 }
//             }
//         }
//     }
// }

fn new_client(svc: &ServiceType) -> anyhow::Result<ClientType> {
    svc.client_builder()
        .initial_max_slice_len(4096)
        .allocation_strategy(AllocationStrategy::PowerOfTwo)
        .unable_to_deliver_strategy(iceoryx2::prelude::UnableToDeliverStrategy::Block)
        .create()
        .context("Failed to create IPC client")
}

fn build_service(
    node: &Node<ipc::Service>,
    service_name: impl AsRef<str>,
) -> anyhow::Result<ServiceType> {
    node.service_builder(&service_name.as_ref().try_into().unwrap())
        .request_response::<[u8], [u8]>()
        .request_user_header::<IORequest>()
        .response_user_header::<IOResponse>()
        // .max_clients(16)
        // .max_active_requests_per_client(16)
        .open_or_create()
        .context("Failed to create IPC service")
}

pub fn new_service(service_name: String) -> anyhow::Result<SvcHandle> {
    set_log_level(LogLevel::Fatal);
    {
        let mut services = SERVICES.lock().unwrap();
        if services.contains_key(&service_name) {
            return Err(anyhow::anyhow!(
                "Service '{}' already exists",
                &service_name,
            ));
        }

        let (tx, rx) = mpsc::channel::<RequestType>();

        let thnd = thread::spawn({
            let service_name = service_name.clone();
            move || {
                println!("Spawned a thread with IPC client");
                let mut node = NODE.lock().unwrap();
                let mut svc =
                    build_service(&node, &service_name).expect("Failed to create IPC service");
                let mut client = new_client(&svc).unwrap();

                while let Ok(req) = rx.recv() {
                    let (io_req, iovecs, resp_tx) = &req;
                    let mut received_response = false;

                    loop {
                        match io_req {
                            read_req @ IORequest::Read { size: _, offset: _ } => {
                                println!("Processing read request: {:?}", read_req);
                                let mut req_data = client.loan_slice_uninit(0).unwrap();
                                let req = req_data.user_header_mut();
                                *req = read_req.clone();
                                let req = unsafe { req_data.assume_init() };
                                let pending_resp = req.send().unwrap();

                                node.wait(CYCLE_TIME).context("Failed to wait").unwrap();

                                if let Some(resp) = pending_resp.receive().transpose() {
                                    let resp = match resp {
                                        Ok(resp) => resp,
                                        Err(e) => {
                                            eprintln!("Failed to receive response: {:?}", e);
                                            break;
                                        }
                                    };
                                    let resp_data = resp.payload();
                                    let resp_header = resp.user_header();

                                    if let IOResponse::Read { size } = *resp_header {
                                        if size >= 0 {
                                            let CIOVecs { iov, iovcnt } = iovecs.expect(
                                                "IOVecs must be provided for read requests",
                                            );
                                            let iovecs = unsafe {
                                                slice::from_raw_parts(iov, iovcnt as usize)
                                            };
                                            let mut buf_pos = 0;
                                            let mut remaining_size = size as usize;
                                            for iov in iovecs {
                                                let buf = unsafe {
                                                    slice::from_raw_parts_mut(
                                                        iov.iov_base as *mut u8,
                                                        iov.iov_len as usize,
                                                    )
                                                };
                                                let buf_size =
                                                    min(iov.iov_len as usize, remaining_size);
                                                buf.copy_from_slice(
                                                    &resp_data[buf_pos..buf_pos + buf_size],
                                                );
                                                buf_pos += iov.iov_len as usize;
                                                remaining_size -= iov.iov_len as usize;
                                            }
                                        }
                                    }

                                    received_response = true;
                                    println!("Sending to response channel: {:?}", resp_header);
                                    resp_tx
                                        .send(resp_header.clone())
                                        .expect("Failed to send response header");
                                } else {
                                    println!("No response received");
                                }
                            }
                            write_req @ IORequest::Write { offset: _ } => {
                                let CIOVecs { iov, iovcnt } =
                                    iovecs.expect("IOVecs must be provided for read requests");
                                let iovecs = unsafe { slice::from_raw_parts(iov, iovcnt as usize) };
                                let total_buf_len =
                                    iovecs.iter().map(|iov| iov.iov_len as usize).sum();

                                let mut req = client.loan_slice_uninit(total_buf_len).unwrap();
                                let req_header = req.user_header_mut();
                                *req_header = write_req.clone();
                                let req_data = req.payload_mut();

                                let mut buf_pos = 0;
                                for iov in iovecs {
                                    // iovec buffer contents should be initialized by the caller
                                    // we just use MaybeUninit type so it is compatible with req_data
                                    let buf = unsafe {
                                        slice::from_raw_parts(
                                            iov.iov_base as *const MaybeUninit<u8>,
                                            iov.iov_len as usize,
                                        )
                                    };
                                    req_data[buf_pos..buf_pos + iov.iov_len as usize]
                                        .copy_from_slice(buf);
                                    buf_pos += iov.iov_len as usize;
                                }

                                let req = unsafe { req.assume_init() };
                                let pending_resp = req.send().unwrap();

                                node.wait(CYCLE_TIME).context("Failed to wait").unwrap();

                                if let Some(resp) = pending_resp.receive().unwrap() {
                                    let resp_header = resp.user_header();

                                    received_response = true;
                                    resp_tx
                                        .send(resp_header.clone())
                                        .expect("Failed to send response header");
                                } else {
                                    println!("No response received");
                                }
                            }
                            size_req @ IORequest::Size => {
                                let mut req_data = client.loan_slice_uninit(0).unwrap();
                                let req = req_data.user_header_mut();
                                *req = size_req.clone();
                                let req = unsafe { req_data.assume_init() };
                                let pending_resp = req.send().unwrap();

                                node.wait(CYCLE_TIME).context("Failed to wait").unwrap();

                                if let Some(resp) = pending_resp.receive().unwrap() {
                                    let resp_header = resp.user_header();

                                    received_response = true;
                                    resp_tx
                                        .send(resp_header.clone())
                                        .expect("Failed to send response header");
                                } else {
                                    println!("No response received");
                                }
                            }
                        }
                        if received_response {
                            break;
                        }
                        // reconnect
                        // println!("Reconnecting to service...");
                        // mem::drop(client);
                        // mem::drop(svc);

                        // *node = NodeBuilder::new()
                        //     .create::<ipc::Service>()
                        //     .expect("Failed to create IPC node");
                        // svc = build_service(&node, &service_name).unwrap();
                        // client = new_client(&svc).unwrap();
                    }
                }
            }
        });

        {
            let mut threads = THREADS.lock().unwrap();
            threads.push(thnd);
        }

        services.insert(service_name.clone(), tx);
    }

    let mut service_names = SERVICE_NAMES.lock().unwrap();
    let handle = SvcHandle(SVC_HND_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    service_names.insert(handle, service_name);

    Ok(handle)
}

pub fn with_client<R>(
    handle: SvcHandle,
    f: impl FnOnce(&RequestSenderType) -> anyhow::Result<R>,
) -> anyhow::Result<R> {
    let tx: RequestSenderType;

    {
        let service_names = SERVICE_NAMES.lock().unwrap();

        let service_name = service_names
            .get(&handle)
            .ok_or_else(|| anyhow::anyhow!("Service handle {:?} not found", handle))?;

        let services = SERVICES.lock().unwrap();

        tx = services
            .get(service_name)
            .ok_or_else(|| anyhow::anyhow!("Service '{}' not found", service_name))?
            .clone();
    }

    f(&tx)
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

pub unsafe fn preadv(hnd: c_int, iov: *const libc::iovec, iovcnt: c_int, offset: off_t) -> ssize_t {
    // println!(
    //     "preadv called with handle: {}, iov: {:?}, iovcnt: {}, offset: {}",
    //     hnd, iov, iovcnt, offset
    // );
    let iovecs = unsafe { slice::from_raw_parts(iov, iovcnt as usize) };
    let total_buf_len = iovecs.iter().map(|iov| iov.iov_len as usize).sum();

    let res = with_client(SvcHandle(hnd), |tx| -> anyhow::Result<ssize_t> {
        let (resp_tx, resp_rx) = mpsc::channel();
        tx.send((
            IORequest::Read {
                size: total_buf_len,
                offset,
            },
            Some(CIOVecs { iov, iovcnt }),
            resp_tx,
        ))?;
        let resp = resp_rx.recv()?;
        // println!("Received response: {:?}", resp);

        match resp {
            IOResponse::Read { size } => {
                if size < 0 {
                    return Err(anyhow::anyhow!("Unexpected read error"));
                }
                return Ok(size);
            }
            IOResponse::Error { errno } => {
                return Err(ErrnoError(errno).into());
            }
            _ => {
                return Err(anyhow::anyhow!("Unexpected response header: {:?}", resp));
            }
        }
    });

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

pub unsafe fn pwritev(
    hnd: c_int,
    iov: *const libc::iovec,
    iovcnt: c_int,
    offset: off_t,
) -> ssize_t {
    let res = with_client(SvcHandle(hnd), |tx| -> anyhow::Result<ssize_t> {
        let (resp_tx, resp_rx) = mpsc::channel();
        tx.send((
            IORequest::Write { offset },
            Some(CIOVecs { iov, iovcnt }),
            resp_tx,
        ))?;
        let resp = resp_rx.recv()?;

        match resp {
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
                return Err(anyhow::anyhow!("Unexpected response header: {:?}", resp));
            }
        }
    });

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

pub unsafe fn size(hnd: c_int) -> ssize_t {
    let res = with_client(SvcHandle(hnd), |tx| -> anyhow::Result<ssize_t> {
        let (resp_tx, resp_rx) = mpsc::channel();
        tx.send((IORequest::Size, None, resp_tx))?;
        let resp = resp_rx.recv()?;

        match resp {
            IOResponse::Size { size } => {
                if size < 0 {
                    return Err(anyhow::anyhow!("Unexpected size error"));
                }
                return Ok(size);
            }
            IOResponse::Error { errno } => {
                return Err(ErrnoError(errno).into());
            }
            _ => {
                return Err(anyhow::anyhow!("Unexpected response header: {:?}", resp));
            }
        }
    });

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
