use iceoryx2::{
    active_request::ActiveRequest,
    prelude::{LogLevel, ZeroCopySend, set_log_level},
    service::ipc,
};
use libc::{c_int, iovec, off_t, size_t, ssize_t};
use std::{fs::File, os::fd::AsRawFd};

pub mod client;
pub mod server;

#[derive(Debug, Clone, ZeroCopySend)]
#[repr(C)]
pub enum IORequest {
    Read { size: size_t, offset: off_t },
    Write { offset: off_t },
    Size,
}

#[derive(Debug, Clone, ZeroCopySend)]
#[repr(C)]
pub enum IOResponse {
    Read { size: ssize_t },
    Write { size: ssize_t },
    Size { size: ssize_t },
    Error { errno: c_int },
}

pub struct IOHandler {
    file: File,
}

impl IOHandler {
    pub fn new(file: File) -> Self {
        IOHandler { file }
    }
}

impl server::Handler for IOHandler {
    type ReqHeader = IORequest;
    type ReqSliceElem = u8;
    type RespHeader = IOResponse;
    type RespSliceElem = u8;

    fn handle_request(
        &self,
        active_request: &ActiveRequest<ipc::Service, [u8], IORequest, [u8], IOResponse>,
    ) -> anyhow::Result<()> {
        let req = active_request.user_header();

        let mut resp_data;

        match *req {
            IORequest::Read { size, offset } => 'read: {
                resp_data = active_request.loan_slice_uninit(size as usize)?;
                let resp_data_ptr = resp_data.payload_mut().as_mut_ptr();
                let resp = resp_data.user_header_mut();

                let iov = iovec {
                    iov_base: resp_data_ptr as *mut _,
                    iov_len: size,
                };
                let size = unsafe { libc::preadv(self.file.as_raw_fd(), &iov, 1, offset) };
                if size < 0 {
                    *resp = IOResponse::Error {
                        errno: std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
                    };
                    break 'read;
                }

                *resp = IOResponse::Read { size };
            }
            IORequest::Write { offset } => 'write: {
                let req_data = active_request.payload();
                resp_data = active_request.loan_slice_uninit(0)?;
                let req_data_ptr = req_data.as_ptr();
                let resp = resp_data.user_header_mut();

                let iov = iovec {
                    iov_base: req_data_ptr as *mut _,
                    iov_len: req_data.len(),
                };
                let size = unsafe { libc::pwritev(self.file.as_raw_fd(), &iov, 1, offset) };
                if size < 0 {
                    *resp = IOResponse::Error {
                        errno: std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
                    };
                    break 'write;
                }
                *resp = IOResponse::Write { size };
            }
            IORequest::Size => 'size: {
                let hnd = self.file.as_raw_fd();
                let mut block_size: u32 = 0;
                let block_size_ptr = &mut block_size as *mut _;
                resp_data = active_request.loan_slice_uninit(0)?;
                let resp = resp_data.user_header_mut();

                if unsafe { libc::ioctl(hnd, 0x40046418, block_size_ptr) } < 0 {
                    *resp = IOResponse::Error {
                        errno: std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
                    };
                    break 'size;
                }
                let mut block_count: u64 = 0;
                let block_count_ptr = &mut block_count as *mut _;

                if unsafe { libc::ioctl(hnd, 0x40086419, block_count_ptr) } < 0 {
                    *resp = IOResponse::Error {
                        errno: std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
                    };
                    break 'size;
                }

                *resp = IOResponse::Size {
                    size: (block_size as u64 * block_count) as ssize_t,
                };
            }
        };
        let resp = unsafe { resp_data.assume_init() };
        resp.send()?;

        Ok(())
    }
}

pub fn start_io_server(service_name: impl AsRef<str>, file: File) -> anyhow::Result<()> {
    set_log_level(LogLevel::Fatal);
    let handler = IOHandler { file };
    let server = server::IOServer::new(service_name, handler)?;
    server.run()
}
