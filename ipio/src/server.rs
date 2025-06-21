use std::fmt;

use iceoryx2::{active_request::ActiveRequest, port::server::Server, prelude::*};

use crate::CYCLE_TIME;

pub trait Handler {
    type ReqSliceElem: fmt::Debug + ZeroCopySend;
    type ReqHeader: fmt::Debug + ZeroCopySend;
    type RespSliceElem: fmt::Debug + ZeroCopySend;
    type RespHeader: fmt::Debug + ZeroCopySend;

    fn handle_request(
        &self,
        active_request: &ActiveRequest<
            ipc::Service,
            [Self::ReqSliceElem],
            Self::ReqHeader,
            [Self::RespSliceElem],
            Self::RespHeader,
        >,
    ) -> anyhow::Result<()>;
}

pub struct IOServer<T: Handler> {
    node: Node<ipc::Service>,
    server:
        Server<ipc::Service, [T::ReqSliceElem], T::ReqHeader, [T::RespSliceElem], T::RespHeader>,
    handler: T,
}

impl<T: Handler> IOServer<T> {
    pub fn new(service_name: impl AsRef<str>, handler: T) -> anyhow::Result<Self> {
        let node = NodeBuilder::new().create::<ipc::Service>()?;

        let service = node
            .service_builder(&service_name.as_ref().try_into()?)
            .request_response::<[T::ReqSliceElem], [T::RespSliceElem]>()
            .request_user_header::<T::ReqHeader>()
            .response_user_header::<T::RespHeader>()
            .max_clients(16)
            .open_or_create()?;
        let server = service
            .server_builder()
            .initial_max_slice_len(4096)
            .allocation_strategy(AllocationStrategy::PowerOfTwo)
            .create()?;

        Ok(IOServer {
            node,
            server,
            handler,
        })
    }

    pub fn run(&self) -> anyhow::Result<()> {
        println!("IPIO server is running...");

        while self.node.wait(CYCLE_TIME).is_ok() {
            while let Some(active_request) = self.server.receive().ok().flatten() {
                // println!("received request: {:?}", active_request);
                println!("received request");
                self.handler.handle_request(&active_request)?;
                println!("sending response");
                // println!("sending response: {:?}", *response);
            }
        }

        Ok(())
    }
}
