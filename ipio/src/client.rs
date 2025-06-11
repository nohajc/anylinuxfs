// there's probably no need of server-side synchronization because Linux kernel should already handle that
// we also need to handle splitting or merging the iovec buffers to achieve vectorized I/O

use std::{
    cell::RefCell,
    collections::{HashMap, hash_map::Entry},
    sync::{LazyLock, Mutex, atomic::AtomicI32},
};

use anyhow::Context;
use iceoryx2::{
    port::client::Client, prelude::*, service::port_factory::request_response::PortFactory,
};

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
    f: impl FnOnce(&mut ClientType) -> R,
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

        Ok(f(client))
    })
}
