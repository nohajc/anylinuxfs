use anyhow::{Context, anyhow};
use base64::prelude::*;
use bincode::{Decode, Encode};
use libc::{c_int, iovec, off_t, size_t, ssize_t};
use nanoid::nanoid;
use std::{
    ffi::{CString, c_void},
    fs::File,
    io::{self, Read, Write},
    mem::MaybeUninit,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::MetadataExt, net::UnixStream},
    },
    process::{Child, Command},
    ptr,
};

pub mod client;
pub mod launcher;

#[derive(Debug, Clone, Encode, Decode)]
#[repr(C)]
pub enum IORequest {
    Read {
        size: size_t,
        offset: off_t,
        shm_size: Option<off_t>,
    },
    Write {
        size: size_t,
        offset: off_t,
        shm_size: Option<off_t>,
    },
    Size,
}

#[derive(Debug, Clone, Encode, Decode)]
#[repr(C)]
pub enum IOResponse {
    Read { size: ssize_t },
    Write { size: ssize_t },
    Size { size: ssize_t },
    Error { errno: c_int },
}

fn unset_cloexec(fd: c_int) -> anyhow::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(anyhow::anyhow!(
            "Failed to get file descriptor flags: {}",
            io::Error::last_os_error()
        ));
    }
    let new_flags = flags & !libc::FD_CLOEXEC;
    if unsafe { libc::fcntl(fd, libc::F_SETFD, new_flags) } < 0 {
        return Err(anyhow::anyhow!(
            "Failed to clear FD_CLOEXEC flag: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub struct Shm {
    fd: c_int,
    size: off_t,
    data: *mut c_void,
}

impl Shm {
    pub fn create_anonymous(size: off_t) -> anyhow::Result<Shm> {
        let name = nanoid!(16);
        let name = CString::new(format!("group.testgrp/{}", name))?;
        let shm_fd = unsafe { libc::shm_open(name.as_ptr(), libc::O_CREAT | libc::O_RDWR, 0o666) };
        if shm_fd < 0 {
            return Err(anyhow::anyhow!(
                "Failed to create shared memory segment: {}",
                io::Error::last_os_error()
            ));
        }

        if let Err(e) = unset_cloexec(shm_fd) {
            unsafe { libc::close(shm_fd) };
            return Err(e);
        }

        let result = unsafe { libc::ftruncate(shm_fd, size) };
        if result < 0 {
            unsafe { libc::close(shm_fd) };
            return Err(anyhow::anyhow!(
                "Failed to set size of shared memory segment: {}",
                io::Error::last_os_error()
            ));
        }

        let result = unsafe { libc::shm_unlink(name.as_ptr() as *const _) };
        if result < 0 {
            unsafe { libc::close(shm_fd) };
            return Err(anyhow::anyhow!(
                "Failed to unlink shared memory segment: {}",
                io::Error::last_os_error()
            ));
        }

        Self::from_fd(shm_fd, size)
    }

    pub fn from_fd(fd: c_int, size: off_t) -> anyhow::Result<Shm> {
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
            unsafe { libc::close(fd) };
            return Err(anyhow::anyhow!(
                "Failed to map shared memory segment: {}",
                io::Error::last_os_error()
            ));
        }

        Ok(Shm { fd, size, data })
    }

    pub fn raw_fd(&self) -> c_int {
        self.fd
    }

    pub fn size(&self) -> off_t {
        self.size
    }

    pub fn resize(&mut self, new_size: off_t, truncate: bool) -> anyhow::Result<()> {
        if new_size <= 0 {
            return Err(anyhow::anyhow!("New size must be greater than zero"));
        }

        if truncate {
            let result = unsafe { libc::ftruncate(self.fd, new_size) };
            if result < 0 {
                return Err(anyhow::anyhow!(
                    "Failed to resize shared memory segment: {}",
                    io::Error::last_os_error()
                ));
            }
        }

        self.unmap()?;

        let data = unsafe {
            libc::mmap(
                self.data,
                new_size as usize,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                self.fd,
                0,
            )
        };
        if data == libc::MAP_FAILED {
            return Err(anyhow::anyhow!(
                "Failed to map shared memory segment: {}",
                io::Error::last_os_error()
            ));
        }

        println!(
            "Resized shared memory segment from {} to {}",
            self.size, new_size
        );
        self.data = data;
        self.size = new_size;
        Ok(())
    }

    fn unmap(&mut self) -> anyhow::Result<()> {
        let result = unsafe { libc::munmap(self.data, self.size as usize) };
        if result < 0 {
            return Err(anyhow::anyhow!(
                "Failed to unmap shared memory segment: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    fn close(&mut self) -> anyhow::Result<()> {
        self.unmap()?;

        let result = unsafe { libc::close(self.fd) };
        if result < 0 {
            return Err(anyhow::anyhow!(
                "Failed to close shared memory file descriptor: {}",
                io::Error::last_os_error()
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

pub struct ServerBuilder {
    shm: Shm,
    sock1: UnixStream,
    sock2: UnixStream,
    hnd_b64: String,
}

pub struct Server {
    pub shm: Shm,
    pub sock: UnixStream,
}

unsafe impl Send for Server {}

pub type BlkDevReadFn = fn(hnd: usize, buf: *mut c_void, offset: i64, size: isize) -> i64;
pub type BlkDevWriteFn = fn(hnd: usize, buf: *mut c_void, offset: i64, size: isize) -> i64;
pub type BlkDevSizeFn = fn(hnd: usize) -> i64;

pub struct IOCallbacks {
    pub read: BlkDevReadFn,
    pub write: BlkDevWriteFn,
    pub size: BlkDevSizeFn,
    pub hnd: usize,
}

pub trait BlkDev {
    fn read(&self, buf: *mut c_void, offset: i64, size: isize) -> i64;
    fn write(&self, buf: *mut c_void, offset: i64, size: isize) -> i64;
    fn size(&self) -> i64;
}

impl BlkDev for IOCallbacks {
    fn read(&self, buf: *mut c_void, offset: i64, size: isize) -> i64 {
        (self.read)(self.hnd, buf, offset, size)
    }

    fn write(&self, buf: *mut c_void, offset: i64, size: isize) -> i64 {
        (self.write)(self.hnd, buf, offset, size)
    }

    fn size(&self) -> i64 {
        (self.size)(self.hnd)
    }
}

impl BlkDev for File {
    fn read(&self, buf: *mut c_void, offset: i64, size: isize) -> i64 {
        let iov = iovec {
            iov_base: buf,
            iov_len: size as usize,
        };
        let size = unsafe { libc::preadv(self.as_raw_fd(), &iov, 1, offset) } as i64;
        if size < 0 {
            -io::Error::last_os_error().raw_os_error().unwrap_or(0) as i64
        } else {
            size
        }
    }

    fn write(&self, buf: *mut c_void, offset: i64, size: isize) -> i64 {
        let iov = iovec {
            iov_base: buf,
            iov_len: size as usize,
        };
        let size = unsafe { libc::pwritev(self.as_raw_fd(), &iov, 1, offset) } as i64;
        if size < 0 {
            -io::Error::last_os_error().raw_os_error().unwrap_or(0) as i64
        } else {
            size
        }
    }

    fn size(&self) -> i64 {
        let size = self.metadata().map(|md| md.size()).ok();
        if let Some(size) = size
            && size > 0
        {
            return size as i64;
        }

        let hnd = self.as_raw_fd();
        let mut block_size: u32 = 0;
        let block_size_ptr = &mut block_size as *mut _;

        if unsafe { libc::ioctl(hnd, 0x40046418, block_size_ptr) } < 0 {
            return -io::Error::last_os_error().raw_os_error().unwrap_or(0) as i64;
        }

        let mut block_count: u64 = 0;
        let block_count_ptr = &mut block_count as *mut _;

        if unsafe { libc::ioctl(hnd, 0x40086419, block_count_ptr) } < 0 {
            return -io::Error::last_os_error().raw_os_error().unwrap_or(0) as i64;
        }

        return (block_size as u64 * block_count) as i64;
    }
}

fn usize_to_b64(n: usize) -> String {
    let b = n.to_le_bytes();
    let len = b
        .iter()
        .rposition(|&byte| byte != 0)
        .map_or(0, |pos| pos + 1);
    BASE64_STANDARD_NO_PAD.encode(&b[..len])
}

fn b64_to_usize(s: &str) -> anyhow::Result<usize> {
    let mut a = [0; 8];
    _ = BASE64_STANDARD_NO_PAD.decode_slice(s, &mut a)?;
    Ok(usize::from_le_bytes(a))
}

impl ServerBuilder {
    pub fn new(hnd: usize, shm_size: off_t) -> anyhow::Result<Self> {
        let (sock1, sock2) = UnixStream::pair().context("Failed to create socket pair")?;
        let shm = Shm::create_anonymous(shm_size).context("Failed to create shared memory")?;
        if let Err(e) = unset_cloexec(sock2.as_raw_fd()) {
            return Err(anyhow::anyhow!("Failed to unset CLOEXEC on sock1: {}", e));
        }
        Ok(ServerBuilder {
            shm,
            sock1,
            sock2,
            hnd_b64: usize_to_b64(hnd),
        })
    }

    pub fn conn_string(&self) -> String {
        let sock2_fd = self.sock2.as_raw_fd();
        let shm_fd = self.shm.raw_fd();
        let shm_size = self.shm.size();
        format!("{}:{}:{}:{}", self.hnd_b64, sock2_fd, shm_fd, shm_size)
    }

    pub fn spawn_client(self, mut cmd: Command) -> anyhow::Result<(Child, Server)> {
        let child = cmd.spawn().context("Failed to spawn client process")?;
        let server = Server {
            shm: self.shm,
            sock: self.sock1,
        };
        Ok((child, server))
    }
}

impl Server {
    pub fn serve(&mut self, blkdev: impl BlkDev) -> anyhow::Result<()> {
        loop {
            let req = match self.recv_request() {
                Ok(req) => req,
                Err(e) => {
                    if let Some(e) = e.downcast_ref::<io::Error>() {
                        if e.kind() == io::ErrorKind::UnexpectedEof {
                            break;
                        }
                    }
                    eprintln!("SERVER: Error receiving request: {}", e);
                    break;
                }
            };
            // println!("SERVER: received request: {:?}", req);
            let resp = match req {
                IORequest::Read {
                    size,
                    offset,
                    shm_size,
                } => {
                    if let Some(new_size) = shm_size {
                        if new_size > self.shm.size() {
                            self.shm.resize(new_size, false)?;
                        }
                    }
                    let size = blkdev.read(self.shm.data, offset, size as isize) as ssize_t;
                    if size < 0 {
                        IOResponse::Error {
                            errno: -size as c_int,
                        }
                    } else {
                        IOResponse::Read { size }
                    }
                }
                IORequest::Write {
                    size,
                    offset,
                    shm_size,
                } => {
                    if let Some(new_size) = shm_size {
                        if new_size > self.shm.size() {
                            self.shm.resize(new_size, false)?;
                        }
                    }
                    let size = blkdev.write(self.shm.data, offset, size as isize) as ssize_t;
                    if size < 0 {
                        IOResponse::Error {
                            errno: -size as c_int,
                        }
                    } else {
                        IOResponse::Write { size }
                    }
                }
                IORequest::Size => {
                    let size = blkdev.size() as ssize_t;
                    if size < 0 {
                        IOResponse::Error {
                            errno: -size as c_int,
                        }
                    } else {
                        IOResponse::Size { size: size }
                    }
                }
            };
            // println!("SERVER: sending response: {:?}", resp);
            self.send_response(resp)?;
            // println!("SERVER: response sent successfully");
        }

        Ok(())
    }

    fn recv_request(&mut self) -> anyhow::Result<IORequest> {
        let mut size_buf = [0u8; 4];
        self.sock
            .read_exact(&mut size_buf)
            .context("Failed to read request size")?;
        let size = u32::from_be_bytes(size_buf) as usize;
        if size == 0 {
            return Err(anyhow!("Request size is zero"));
        }
        if size > 4096 {
            return Err(anyhow!("Request size is too large"));
        }

        let mut payload_buf = vec![0u8; size];
        self.sock
            .read_exact(&mut payload_buf)
            .context("Failed to read request payload")?;

        let (req, _) = bincode::decode_from_slice(&payload_buf, bincode::config::standard())?;
        Ok(req)
    }

    fn send_response(&mut self, response: IOResponse) -> anyhow::Result<()> {
        let response_buf = bincode::encode_to_vec(&response, bincode::config::standard())?;
        let size = response_buf.len() as u32;
        let size_buf = size.to_be_bytes();
        self.sock
            .write_all(&size_buf)
            .context("Failed to write response size")?;
        self.sock
            .write_all(&response_buf)
            .context("Failed to write response payload")?;

        Ok(())
    }
}

pub struct Client {
    pub shm: Shm,
    pub sock: UnixStream,
    pub hnd: usize,
}

unsafe impl Send for Client {}

impl Client {
    pub unsafe fn from_conn_string(conn_string: &str) -> anyhow::Result<Self> {
        let parts: Vec<&str> = conn_string.split(':').collect();
        if parts.len() != 4 {
            return Err(anyhow::anyhow!("Invalid connection string format"));
        }

        let hnd: usize =
            b64_to_usize(parts[0]).context("Failed to parse handle from connection string")?;

        let sock_fd: c_int = parts[1]
            .parse()
            .context("Failed to parse socket file descriptor")?;
        let shm_fd: c_int = parts[2]
            .parse()
            .context("Failed to parse shared memory file descriptor")?;
        let shm_size: off_t = parts[3]
            .parse()
            .context("Failed to parse shared memory size")?;

        let sock = unsafe { UnixStream::from_raw_fd(sock_fd) };
        let shm = Shm::from_fd(shm_fd, shm_size)?;

        Ok(Client { shm, sock, hnd })
    }

    pub fn send_request(&mut self, request: IORequest) -> anyhow::Result<()> {
        let request_buf = bincode::encode_to_vec(&request, bincode::config::standard())?;
        let size = request_buf.len() as u32;
        let size_buf = size.to_be_bytes();
        self.sock
            .write_all(&size_buf)
            .context("Failed to write request size")?;
        self.sock
            .write_all(&request_buf)
            .context("Failed to write request payload")?;

        Ok(())
    }

    pub fn recv_response(&mut self) -> anyhow::Result<IOResponse> {
        let mut size_buf = [0u8; 4];
        self.sock
            .read_exact(&mut size_buf)
            .context("Failed to read response size")?;
        let size = u32::from_be_bytes(size_buf) as usize;
        if size == 0 {
            return Err(anyhow!("Response size is zero"));
        }
        if size > 4096 {
            return Err(anyhow!("Response size is too large"));
        }

        let mut payload_buf = vec![0u8; size];
        self.sock
            .read_exact(&mut payload_buf)
            .context("Failed to read response payload")?;

        let (resp, _) = bincode::decode_from_slice(&payload_buf, bincode::config::standard())?;
        Ok(resp)
    }
}
