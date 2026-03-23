use std::{
    net::Ipv4Addr,
    os::unix::{
        fs::chown,
        net::{UnixListener, UnixStream},
    },
    path::Path,
    sync::{Arc, Mutex},
    thread,
};

use anyhow::Context;
use common_utils::{
    host_eprintln,
    ipc::{Client, Handler},
};
use serde::{Deserialize, Serialize};

use crate::{devinfo::DevInfo, settings::MountConfig};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RuntimeInfo {
    pub mount_config: MountConfig,
    pub dev_info: DevInfo,
    pub session_pgid: libc::pid_t,
    pub vmm_pid: libc::pid_t,
    pub net_helper_pid: libc::pid_t,
    pub vm_host: Vec<u8>,
    pub vm_native_ip: Option<Ipv4Addr>,
    pub mount_point: Option<String>,
}

pub fn serve_info(rt_info: Arc<Mutex<RuntimeInfo>>, socket_path: String) {
    _ = thread::spawn(move || {
        let path = Path::new(&socket_path);
        if let Err(e) = UnixHandler::serve(rt_info, path) {
            host_eprintln!("Error in serve_config: {}", e);
        }
    });
}

#[derive(Clone, Deserialize, Serialize)]
pub enum Request {
    GetConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Response {
    Config(RuntimeInfo),
}

struct UnixHandler {}

impl UnixHandler {
    fn serve(runtime_info: Arc<Mutex<RuntimeInfo>>, socket_path: &Path) -> anyhow::Result<()> {
        let listener = UnixListener::bind(socket_path).context("Failed to bind to Unix socket")?;

        {
            let rt_info = runtime_info.lock().unwrap();
            chown(
                socket_path,
                Some(rt_info.mount_config.common.invoker_uid),
                Some(rt_info.mount_config.common.invoker_gid),
            )
            .context(format!(
                "Failed to change owner of {}",
                socket_path.display(),
            ))?;
        }

        for stream in listener.incoming() {
            let Ok(stream) = stream else {
                continue;
            };
            _ = UnixHandler::serve_to_client(stream, runtime_info.clone());
        }

        Ok(())
    }

    fn serve_to_client(
        mut stream: UnixStream,
        runtime_info: Arc<Mutex<RuntimeInfo>>,
    ) -> anyhow::Result<()> {
        let req = Handler::read_request(&mut stream)?;
        let resp = match req {
            Request::GetConfig => Response::Config(runtime_info.lock().unwrap().clone()),
        };
        Handler::write_response(&mut stream, &resp)?;

        Ok(())
    }
}

pub struct UnixClient {}

impl UnixClient {
    pub fn make_request(socket_path: &str, req: Request) -> anyhow::Result<Response> {
        let mut stream = UnixStream::connect(socket_path).context("Failed to connect to socket")?;
        Client::write_request(&mut stream, &req)?;
        let resp = Client::read_response(&mut stream)?;
        Ok(resp)
    }
}
