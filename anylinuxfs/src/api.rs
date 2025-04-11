use std::{
    fs,
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::Path,
    thread,
};

use anyhow::Context;
use common_utils::host_eprintln;
use serde::{Deserialize, Serialize};

use crate::{MountConfig, devinfo::DevInfo};

const API_SOCKET: &str = "/tmp/anylinuxfs.sock";

pub fn serve_info(mount_config: &MountConfig, dev_info: &DevInfo) {
    let mount_config = mount_config.clone();
    let dev_info = dev_info.clone();

    _ = thread::spawn(move || {
        let socket_path = Path::new(API_SOCKET);
        // Remove the socket file if it exists
        if socket_path.exists() {
            if let Err(e) = fs::remove_file(socket_path) {
                host_eprintln!("Error removing socket file: {}", e);
            }
        }
        if let Err(e) = Handler::serve(
            RuntimeInfo {
                mount_config,
                dev_info,
            },
            socket_path,
        ) {
            host_eprintln!("Error in serve_config: {}", e);
        }
    });
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RuntimeInfo {
    mount_config: MountConfig,
    dev_info: DevInfo,
}

#[derive(Clone, Deserialize, Serialize)]
pub enum Request {
    GetConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Response {
    Config(RuntimeInfo),
}

struct Handler {}

impl Handler {
    fn serve(runtime_info: RuntimeInfo, socket_path: &Path) -> anyhow::Result<()> {
        let listener = UnixListener::bind(socket_path).context("Failed to bind to Unix socket")?;

        for stream in listener.incoming() {
            let Ok(stream) = stream else {
                continue;
            };
            _ = Handler::serve_to_client(stream, runtime_info.clone());
        }

        Ok(())
    }

    fn serve_to_client(mut stream: UnixStream, runtime_info: RuntimeInfo) -> anyhow::Result<()> {
        let req = Handler::read_request(&mut stream)?;
        let resp = match req {
            Request::GetConfig => Response::Config(runtime_info),
        };
        Handler::write_response(&mut stream, resp)?;

        Ok(())
    }

    fn read_request(stream: &mut UnixStream) -> anyhow::Result<Request> {
        let mut size_buf = [0u8; 4];
        stream
            .read_exact(&mut size_buf)
            .context("Failed to read request size")?;
        let size = u32::from_be_bytes(size_buf) as usize;
        if size == 0 {
            return Err(anyhow::anyhow!("Request size is zero"));
        }
        if size > 4096 {
            return Err(anyhow::anyhow!("Request size is too large"));
        }

        let mut payload_buf = vec![0u8; size];
        stream
            .read_exact(&mut payload_buf)
            .context("Failed to read request payload")?;

        let request: Request =
            serde_json::from_slice(&payload_buf).context("Failed to parse request")?;
        Ok(request)
    }

    fn write_response(stream: &mut UnixStream, response: Response) -> anyhow::Result<()> {
        let response_str =
            serde_json::to_string(&response).context("Failed to serialize response")?;
        let size = response_str.len() as u32;
        let size_buf = size.to_be_bytes();
        stream
            .write_all(&size_buf)
            .context("Failed to write response size")?;
        stream
            .write_all(response_str.as_bytes())
            .context("Failed to write response payload")?;

        Ok(())
    }
}

pub struct Client {}

impl Client {
    pub fn make_request(req: Request) -> anyhow::Result<Response> {
        let mut stream = UnixStream::connect(API_SOCKET).context("Failed to connect to socket")?;
        Client::write_request(&mut stream, req)?;
        let resp = Client::read_response(&mut stream)?;
        Ok(resp)
    }

    fn write_request(stream: &mut UnixStream, request: Request) -> anyhow::Result<()> {
        let request_str = serde_json::to_string(&request).context("Failed to serialize request")?;
        let size = request_str.len() as u32;
        let size_buf = size.to_be_bytes();
        stream
            .write_all(&size_buf)
            .context("Failed to write request size")?;
        stream
            .write_all(request_str.as_bytes())
            .context("Failed to write request payload")?;

        Ok(())
    }

    fn read_response(stream: &mut UnixStream) -> anyhow::Result<Response> {
        let mut size_buf = [0u8; 4];
        stream
            .read_exact(&mut size_buf)
            .context("Failed to read response size")?;
        let size = u32::from_be_bytes(size_buf) as usize;
        if size == 0 {
            return Err(anyhow::anyhow!("Response size is zero"));
        }
        if size > 4096 {
            return Err(anyhow::anyhow!("Response size is too large"));
        }

        let mut payload_buf = vec![0u8; size];
        stream
            .read_exact(&mut payload_buf)
            .context("Failed to read response payload")?;

        let response: Response =
            serde_json::from_slice(&payload_buf).context("Failed to parse response")?;
        Ok(response)
    }
}
