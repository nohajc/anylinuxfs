use iceoryx2::{
    active_request::ActiveRequest,
    prelude::{LogLevel, ZeroCopySend, set_log_level},
    service::ipc,
};
use libc::{c_int, iovec, off_t, size_t, ssize_t};
use nanoid::nanoid;
use std::{
    ffi::{CString, c_void},
    fs::File,
    mem::MaybeUninit,
    os::fd::AsRawFd,
    ptr,
};

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

#[derive(Debug)]
pub struct Shm {
    fd: c_int,
    size: off_t,
    data: *mut c_void,
}

pub fn shm_create_anonymous(size: off_t) -> anyhow::Result<Shm> {
    let name = nanoid!(16);
    let name = CString::new(format!("{}", name))?;
    let shm_fd = unsafe { libc::shm_open(name.as_ptr(), libc::O_CREAT | libc::O_RDWR, 0o666) };
    if shm_fd < 0 {
        return Err(anyhow::anyhow!(
            "Failed to create shared memory segment: {}",
            std::io::Error::last_os_error()
        ));
    }

    let flags = unsafe { libc::fcntl(shm_fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(anyhow::anyhow!(
            "Failed to get file descriptor flags: {}",
            std::io::Error::last_os_error()
        ));
    }
    let new_flags = flags & !libc::FD_CLOEXEC;
    if unsafe { libc::fcntl(shm_fd, libc::F_SETFD, new_flags) } < 0 {
        return Err(anyhow::anyhow!(
            "Failed to clear FD_CLOEXEC flag: {}",
            std::io::Error::last_os_error()
        ));
    }

    let result = unsafe { libc::ftruncate(shm_fd, size) };
    if result < 0 {
        return Err(anyhow::anyhow!(
            "Failed to set size of shared memory segment: {}",
            std::io::Error::last_os_error()
        ));
    }

    let result = unsafe { libc::shm_unlink(name.as_ptr() as *const _) };
    if result < 0 {
        return Err(anyhow::anyhow!(
            "Failed to unlink shared memory segment: {}",
            std::io::Error::last_os_error()
        ));
    }

    shm_from_fd(shm_fd, size)
}

pub fn shm_from_fd(fd: c_int, size: off_t) -> anyhow::Result<Shm> {
    let data = unsafe {
        libc::mmap(
            ptr::null_mut(),
            size as usize,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    if data == libc::MAP_FAILED {
        return Err(anyhow::anyhow!(
            "Failed to map shared memory segment: {}",
            std::io::Error::last_os_error()
        ));
    }

    Ok(Shm { fd, size, data })
}

impl Shm {
    pub fn raw_fd(&self) -> c_int {
        self.fd
    }

    pub fn size(&self) -> off_t {
        self.size
    }

    fn close(&self) -> anyhow::Result<()> {
        let result = unsafe { libc::munmap(self.data, self.size as usize) };
        if result < 0 {
            return Err(anyhow::anyhow!(
                "Failed to unmap shared memory segment: {}",
                std::io::Error::last_os_error()
            ));
        }

        let result = unsafe { libc::close(self.fd) };
        if result < 0 {
            return Err(anyhow::anyhow!(
                "Failed to close shared memory file descriptor: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    pub unsafe fn data(&self) -> &mut [MaybeUninit<u8>] {
        unsafe { std::slice::from_raw_parts_mut(self.data as *mut _, self.size as usize) }
    }
}

impl Drop for Shm {
    fn drop(&mut self) {
        if let Err(e) = self.close() {
            eprintln!("Error closing shared memory (fd {}): {}", self.raw_fd(), e);
        }
    }
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
