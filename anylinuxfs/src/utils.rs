use std::{
    cell::Cell,
    collections::HashSet,
    error::Error,
    ffi::{CString, c_void},
    fs::{File, Permissions},
    io::{self, BufRead, BufReader, Read, Write},
    mem::ManuallyDrop,
    net::IpAddr,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::PermissionsExt,
    },
    path::Path,
    process::Child,
    ptr::null,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, anyhow};
use bstr::{BStr, BString, ByteSlice, ByteVec};
use common_utils::{
    host_println,
    log::{self, Prefix},
    prefix_eprintln, prefix_print, prefix_println, safe_print,
};
use crossterm::event::{self, Event};
use nix::{
    sys::signal::Signal,
    unistd::{Uid, User},
};
use objc2_core_foundation::{
    CFCopyTypeIDDescription, CFDictionary, CFGetTypeID, CFRetained, CFString, CFType,
};
use signal_hook::{consts::TERM_SIGNALS, iterator::Signals};

use crate::pubsub::{PubSub, Subscription};

#[derive(Debug)]
pub struct StatusError {
    pub msg: String,
    pub status: libc::c_int,
}

impl StatusError {
    pub fn new(msg: &str, status: libc::c_int) -> Self {
        Self {
            msg: msg.to_owned(),
            status,
        }
    }
}

impl Error for StatusError {}

impl std::fmt::Display for StatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.msg, self.status)
    }
}

pub fn start_signal_publisher() -> anyhow::Result<PubSub<libc::c_int>> {
    let hub = PubSub::new();
    let mut signals = Signals::new(TERM_SIGNALS).context("failed to register signals")?;
    _ = thread::spawn({
        let hub = hub.clone();
        move || {
            for signal in signals.forever() {
                host_println!(
                    "Received signal {}",
                    Signal::try_from(signal)
                        .map(|s| s.to_string())
                        .unwrap_or("<unknown>".to_owned())
                );
                hub.publish(signal);
            }
        }
    });

    Ok(hub)
}

pub struct CommFd {
    fd: libc::c_int,
}
pub struct PtyFd {
    master_fd: libc::c_int,
}
pub struct PipeOutFds {
    out_fd: libc::c_int,
    err_fd: libc::c_int,
}

pub struct PipeInFd {
    in_fd: libc::c_int,
}

pub trait HasCommFd {
    fn comm_fd(&self) -> libc::c_int;
}

impl HasCommFd for CommFd {
    fn comm_fd(&self) -> libc::c_int {
        self.fd
    }
}

pub trait HasPtyFd {
    fn master_fd(&self) -> libc::c_int;
}

impl HasPtyFd for PtyFd {
    fn master_fd(&self) -> libc::c_int {
        self.master_fd
    }
}

pub trait HasPipeOutFds {
    fn out_fd(&self) -> libc::c_int;
    fn err_fd(&self) -> libc::c_int;
}

impl HasPipeOutFds for PipeOutFds {
    fn out_fd(&self) -> libc::c_int {
        self.out_fd
    }
    fn err_fd(&self) -> libc::c_int {
        self.err_fd
    }
}

pub trait HasPipeInFd {
    fn in_fd(&self) -> libc::c_int;
}

impl HasPipeInFd for PipeInFd {
    fn in_fd(&self) -> libc::c_int {
        self.in_fd
    }
}

pub struct ForkOutput<O, I = (), C = ()> {
    pub pid: libc::pid_t,
    pub out_fds: O,
    pub in_fds: I,
    pub ctrl_fds: C,
    redirect_action: Option<Box<dyn FnOnce() -> anyhow::Result<()>>>,
}

impl<O, I, C> ForkOutput<O, I, C> {
    pub fn redirect(&mut self) -> anyhow::Result<()> {
        if let Some(redirect_fn) = self.redirect_action.take() {
            redirect_fn()?;
        }
        Ok(())
    }
}

impl<O, I> HasCommFd for ForkOutput<O, I, CommFd> {
    fn comm_fd(&self) -> libc::c_int {
        self.ctrl_fds.comm_fd()
    }
}

impl<I, C> HasPtyFd for ForkOutput<PtyFd, I, C> {
    fn master_fd(&self) -> libc::c_int {
        self.out_fds.master_fd()
    }
}

impl<I, C> HasPipeOutFds for ForkOutput<PipeOutFds, I, C> {
    fn out_fd(&self) -> libc::c_int {
        self.out_fds.out_fd()
    }
    fn err_fd(&self) -> libc::c_int {
        self.out_fds.err_fd()
    }
}

impl<O, C> HasPipeInFd for ForkOutput<O, PipeInFd, C> {
    fn in_fd(&self) -> libc::c_int {
        self.in_fds.in_fd()
    }
}

#[allow(unused)]
pub enum OutputAction {
    RedirectNow,
    RedirectLater,
}

pub fn fork_with_pty_output(out_action: OutputAction) -> anyhow::Result<ForkOutput<PtyFd>> {
    let mut master_fd: libc::c_int = 0;
    let mut slave_fd: libc::c_int = 0;

    // let (child_in_read_fd, child_in_write_fd) = new_pipe()?;

    // Create a new pseudo-terminal
    let mut winp: libc::winsize = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let res = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut winp,
        )
    };
    if res < 0 {
        return Err(io::Error::last_os_error()).context("Failed to create pseudo-terminal");
    }

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error()).context("Failed to fork process");
    } else if pid == 0 {
        // Child process

        // Close the write end of the pipe
        // let res = unsafe { libc::close(child_in_write_fd) };
        // if res < 0 {
        //     return Err(io::Error::last_os_error()).context("Failed to close write end of pipe");
        // }

        // Close the master end of the pty
        let res = unsafe { libc::close(master_fd) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to close master end of pty");
        }

        let redirect_fn = move || -> anyhow::Result<()> {
            // Redirect stdout and stderr to the slave end of the pty
            let res = unsafe { libc::dup2(slave_fd, libc::STDOUT_FILENO) };
            if res < 0 {
                return Err(io::Error::last_os_error()).context("Failed to redirect stdout to pty");
            }
            let res = unsafe { libc::dup2(slave_fd, libc::STDERR_FILENO) };
            if res < 0 {
                return Err(io::Error::last_os_error()).context("Failed to redirect stderr to pty");
            }

            // Redirect stdin to the slave end of the pty
            let res = unsafe { libc::dup2(slave_fd, libc::STDIN_FILENO) };
            if res < 0 {
                return Err(io::Error::last_os_error()).context("Failed to redirect stdin to pty");
            }

            // Close the slave end of the pty
            let res = unsafe { libc::close(slave_fd) };
            if res < 0 {
                return Err(io::Error::last_os_error()).context("Failed to close slave end of pty");
            }

            // // Redirect stdin to the read end of the pipe
            // // (we need this initially for interacting with cryptsetup while augmenting the prompts)
            // let res = unsafe { libc::dup2(child_in_read_fd, libc::STDIN_FILENO) };
            // if res < 0 {
            //     return Err(io::Error::last_os_error()).context("Failed to redirect stdin");
            // }

            // // Close the read end of the pipe
            // let res = unsafe { libc::close(child_in_read_fd) };
            // if res < 0 {
            //     return Err(io::Error::last_os_error()).context("Failed to close read end of pipe");
            // }

            Ok(())
        };

        let redirect_action = match out_action {
            OutputAction::RedirectNow => {
                redirect_fn()?;
                None
            }
            OutputAction::RedirectLater => {
                Some(Box::new(redirect_fn) as Box<dyn FnOnce() -> anyhow::Result<()>>)
            }
        };

        Ok(ForkOutput {
            pid,
            out_fds: PtyFd { master_fd: -1 },
            // in_fds: PipeInFd {
            //     in_fd: child_in_write_fd,
            // },
            in_fds: (),
            ctrl_fds: (),
            redirect_action,
        })
    } else {
        // Parent process

        // Close the slave end of the pty
        let res = unsafe { libc::close(slave_fd) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to close slave end of pty");
        }

        Ok(ForkOutput {
            pid,
            out_fds: PtyFd { master_fd },
            // in_fds: PipeInFd {
            //     in_fd: child_in_read_fd,
            // },
            in_fds: (),
            ctrl_fds: (),
            redirect_action: None,
        })
    }
}

fn new_pipe() -> anyhow::Result<(libc::c_int, libc::c_int)> {
    let mut fds: [libc::c_int; 2] = [0; 2];
    let res = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if res < 0 {
        return Err(io::Error::last_os_error()).context("Failed to create pipe");
    }
    Ok((fds[0], fds[1]))
}

pub fn fork_with_piped_output() -> anyhow::Result<ForkOutput<PipeOutFds, PipeInFd>> {
    let (child_out_read_fd, child_out_write_fd) = new_pipe()?;
    let (child_err_read_fd, child_err_write_fd) = new_pipe()?;
    let (child_in_read_fd, child_in_write_fd) = new_pipe()?;

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error()).context("Failed to fork process");
    } else if pid == 0 {
        // Child process

        // Close the write end of the pipe
        let res = unsafe { libc::close(child_in_write_fd) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to close write end of pipe");
        }

        // Close the read end of the pipe
        let res = unsafe { libc::close(child_out_read_fd) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to close read end of pipe");
        }
        let res = unsafe { libc::close(child_err_read_fd) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to close read end of pipe");
        }

        // Redirect stdin to the read end of the pipe
        let res = unsafe { libc::dup2(child_in_read_fd, libc::STDIN_FILENO) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to redirect stdin");
        }

        // Close the read end of the pipe
        let res = unsafe { libc::close(child_in_read_fd) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to close read end of pipe");
        }

        // Redirect stdout and stderr to the write end of each pipe
        let res = unsafe { libc::dup2(child_out_write_fd, libc::STDOUT_FILENO) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to redirect stdout");
        }
        let res = unsafe { libc::dup2(child_err_write_fd, libc::STDERR_FILENO) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to redirect stderr");
        }

        // Close the write end of the pipe
        let res = unsafe { libc::close(child_out_write_fd) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to close write end of pipe");
        }
        let res = unsafe { libc::close(child_err_write_fd) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to close write end of pipe");
        }
    } else {
        // Parent process

        // Close the read end of the pipe
        let res = unsafe { libc::close(child_in_read_fd) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to close read end of pipe");
        }

        // Close the write end of the pipe
        let res = unsafe { libc::close(child_out_write_fd) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to close write end of pipe");
        }
        let res = unsafe { libc::close(child_err_write_fd) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to close write end of pipe");
        }
    }

    Ok(ForkOutput {
        pid,
        out_fds: PipeOutFds {
            out_fd: child_out_read_fd,
            err_fd: child_err_read_fd,
        },
        in_fds: PipeInFd {
            in_fd: child_in_write_fd,
        },
        ctrl_fds: (),
        redirect_action: None,
    })
}

// pipe_fd contains the read end of the pipe in the parent
// and the write end of the pipe in the child process
pub fn fork_with_comm_pipe() -> anyhow::Result<ForkOutput<(), (), CommFd>> {
    let (parent_read_fd, child_write_fd) = new_pipe()?;

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error()).context("Failed to fork process");
    } else if pid == 0 {
        // Child process

        // Close the read end of the pipe
        let res = unsafe { libc::close(parent_read_fd) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to close read end of pipe");
        }

        Ok(ForkOutput {
            pid,
            out_fds: (),
            in_fds: (),
            ctrl_fds: CommFd { fd: child_write_fd },
            redirect_action: None,
        })
    } else {
        // Parent process

        // Close the write end of the pipe
        let res = unsafe { libc::close(child_write_fd) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to close write end of pipe");
        }

        Ok(ForkOutput {
            pid,
            out_fds: (),
            in_fds: (),
            ctrl_fds: CommFd { fd: parent_read_fd },
            redirect_action: None,
        })
    }
}

#[allow(unused)]
pub fn redirect_to_null(fd: libc::c_int) -> anyhow::Result<()> {
    let dev_null_fd = unsafe { libc::open(b"/dev/null\0".as_ptr() as *const i8, libc::O_RDONLY) };
    if dev_null_fd < 0 {
        return Err(io::Error::last_os_error()).context("Failed to open /dev/null");
    }
    let res = unsafe { libc::dup2(dev_null_fd, fd) };
    if res < 0 {
        return Err(io::Error::last_os_error()).context("Failed to redirect fd to /dev/null");
    }
    let res = unsafe { libc::close(dev_null_fd) };
    if res < 0 {
        return Err(io::Error::last_os_error()).context("Failed to close /dev/null fd");
    }

    Ok(())
}

pub unsafe fn write_to_pipe(pipe_fd: libc::c_int, data: &[u8]) -> anyhow::Result<()> {
    let mut f = ManuallyDrop::new(unsafe { File::from_raw_fd(pipe_fd) });
    f.write_all(data).context("Failed to write to pipe")?;
    Ok(())
}

pub enum FlockKind {
    Shared,
    Exclusive,
}

pub struct LockFile(File);

impl LockFile {
    pub fn new(file_path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let file_already_existed = file_path.as_ref().exists();
        let file = File::create(file_path).context("Failed to create lock file")?;
        if !file_already_existed {
            file.set_permissions(Permissions::from_mode(0o666))
                .context("Failed to set file lock permissions")?;
        }

        Ok(Self(file))
    }
}

impl AsRawFd for LockFile {
    fn as_raw_fd(&self) -> libc::c_int {
        self.0.as_raw_fd()
    }
}

fn acquire_lock(file: &impl AsRawFd, lock_kind: FlockKind, err_msg: &str) -> anyhow::Result<()> {
    let lock_flag = match lock_kind {
        FlockKind::Shared => libc::LOCK_SH,
        FlockKind::Exclusive => libc::LOCK_EX,
    };

    // Try to lock the file
    let res = unsafe { libc::flock(file.as_raw_fd(), lock_flag | libc::LOCK_NB) };
    if res != 0 {
        Err(io::Error::new(io::ErrorKind::AlreadyExists, err_msg).into())
    } else {
        Ok(())
    }
}

pub trait AcquireLock: Sized {
    fn acquire_lock(self, lock_kind: FlockKind) -> anyhow::Result<Self>;
}

impl AcquireLock for LockFile {
    fn acquire_lock(self, lock_kind: FlockKind) -> anyhow::Result<LockFile> {
        acquire_lock(&self, lock_kind, "another instance is already running")?;
        Ok(self)
    }
}

impl AcquireLock for File {
    fn acquire_lock(self, lock_kind: FlockKind) -> anyhow::Result<File> {
        acquire_lock(&self, lock_kind, "file already locked")?;
        Ok(self)
    }
}

pub fn try_port(ip: impl Into<IpAddr>, port: u16) -> io::Result<()> {
    std::net::TcpListener::bind((ip.into(), port)).map(|_| ())
}

pub fn check_port_availability(ip: impl Into<IpAddr>, port: u16) -> anyhow::Result<()> {
    try_port(ip.into(), port).map_err(|e| {
        if e.kind() == io::ErrorKind::AddrInUse {
            anyhow!("port {port} already in use")
        } else {
            anyhow!("unexpected error checking port {port}: {e}")
        }
    })
}

pub unsafe fn cfdict_get_value<'a, T>(dict: &'a CFDictionary, key: &str) -> Option<&'a T> {
    let key = CFString::from_str(key);
    let key_ptr: *const CFString = unsafe { CFRetained::as_ptr(&key).as_ref() };
    let mut value_ptr: *const c_void = null();
    let key_found = unsafe { dict.value_if_present(key_ptr as *const c_void, &mut value_ptr) };

    if !key_found {
        return None;
    }
    unsafe { (value_ptr as *const T).as_ref() }
}

#[allow(unused)]
pub fn inspect_cf_dictionary_values(dict: &CFDictionary) {
    let count = dict.count() as usize;
    let mut keys: Vec<*const c_void> = vec![null(); count];
    let mut values: Vec<*const c_void> = vec![null(); count];

    unsafe { dict.keys_and_values(keys.as_mut_ptr(), values.as_mut_ptr()) };

    for i in 0..count {
        let value = values[i] as *const CFType;
        let type_id = unsafe { CFGetTypeID(value.as_ref()) };
        let type_name = CFCopyTypeIDDescription(type_id).unwrap();
        let key_str = keys[i] as *const CFString;

        host_println!(
            "Key: {}, Type: {}",
            unsafe { key_str.as_ref().unwrap() },
            &type_name,
        );
    }
}

pub trait ToPtrVec {
    fn to_ptr_vec(self) -> Vec<*const libc::c_char>;
}

impl ToPtrVec for &[CString] {
    fn to_ptr_vec(self) -> Vec<*const libc::c_char> {
        self.iter()
            .map(|s| s.as_ptr())
            .chain([std::ptr::null()])
            .collect()
    }
}

pub trait ToCStringVec {
    fn to_cstring_vec(self) -> Vec<CString>;
}

impl ToCStringVec for &[String] {
    fn to_cstring_vec(self) -> Vec<CString> {
        self.iter()
            .map(|s| CString::new(s.as_str()).unwrap())
            .collect()
    }
}

impl ToCStringVec for &[BString] {
    fn to_cstring_vec(self) -> Vec<CString> {
        self.iter()
            .map(|s| CString::new(s.as_slice()).unwrap())
            .collect()
    }
}

pub fn find_env_vars(expression: impl AsRef<BStr>) -> HashSet<BString> {
    let mut vars = HashSet::new();
    let mut in_var = false;
    let mut in_braces = false;
    let mut var_name = BString::default();

    let mut last_char = '^';
    for c in expression.as_ref().chars() {
        match (in_var, in_braces, c) {
            // When we're in a variable with braces and encounter closing brace
            (true, true, '}') => {
                if !var_name.is_empty() {
                    vars.insert(var_name.clone());
                    var_name.clear();
                }
                in_var = false;
                in_braces = false;
            }
            // When we're in a variable with braces and encounter valid identifier chars
            (true, true, c) if c.is_alphanumeric() || c == '_' => {
                var_name.push_char(c);
            }
            // When we're in a variable with braces but encounter invalid chars (error case)
            (true, true, _) => {
                // Invalid character inside ${}, ignore and reset
                var_name.clear();
                in_var = false;
                in_braces = false;
            }
            // When we're in a variable without braces and encounter another $
            (true, false, '$') => {
                if !var_name.is_empty() {
                    vars.insert(var_name.clone());
                    var_name.clear();
                }
            }
            // When we're in a variable without braces and encounter opening brace (only if var_name is empty)
            (true, false, '{') if var_name.is_empty() => {
                in_braces = true;
            }
            // When we're in a variable without braces and encounter valid identifier chars
            (true, false, c) if c.is_alphanumeric() || c == '_' => {
                var_name.push_char(c);
            }
            // When we're in a variable without braces and encounter invalid chars
            (true, false, _) => {
                if !var_name.is_empty() {
                    vars.insert(var_name.clone());
                    var_name.clear();
                }
                in_var = false;
            }
            // When we're not in a variable and encounter $ (start of variable)
            (false, false, '$') => {
                if last_char != '\\' {
                    in_var = true;
                }
            }
            // When we're not in a variable and encounter any other char, ignore
            (false, false, _) => {}
            // Invalid state: not in variable but in_braces is true (should never happen)
            (false, true, _) => {
                // Reset to safe state
                in_braces = false;
            }
        }
        last_char = c;
    }

    // Handle case where expression ends while we're in a variable (only for non-braced vars)
    if in_var && !in_braces && !var_name.is_empty() {
        vars.insert(var_name);
    }

    vars
}

pub fn user_name_from_uid(uid: libc::uid_t) -> Option<String> {
    User::from_uid(Uid::from_raw(uid))
        .ok()
        .flatten()
        .map(|u| u.name)
}

/// A buffered reader that immediately outputs characters as they arrive
/// while also providing line-based reading for pattern matching
pub struct PassthroughBufReader<R> {
    inner: R,
    buffer: Vec<u8>,
    pos: usize,
    cap: usize,
    suppress_current_line: bool,
    pending_output: String, // Buffer for chunks we haven't output yet
    first_chunk: bool,      // Track if this is the first chunk on a new line
    guest_prefix: Prefix,
}

impl<R: Read> PassthroughBufReader<R> {
    pub fn new(inner: R, guest_prefix: Prefix) -> Self {
        Self {
            inner,
            buffer: vec![0; 8192],
            pos: 0,
            cap: 0,
            suppress_current_line: false,
            pending_output: String::new(),
            first_chunk: true,
            guest_prefix,
        }
    }

    fn select_prefix(&self, first_chunk: bool) -> Option<Prefix> {
        if first_chunk {
            Some(self.guest_prefix)
        } else {
            None
        }
    }

    pub fn read_line(&mut self, line: &mut String) -> io::Result<usize> {
        let mut total_read = 0;
        line.clear();

        loop {
            // If we need more data, read it
            if self.pos >= self.cap {
                self.pos = 0;
                self.cap = self.inner.read(&mut self.buffer)?;
                if self.cap == 0 {
                    // EOF - output any pending content
                    if !self.pending_output.is_empty() {
                        let prefix = self.select_prefix(self.first_chunk);
                        prefix_print!(prefix, "{}", self.pending_output);
                        self.first_chunk = false;
                        io::stdout().flush().unwrap_or(());
                        self.pending_output.clear();
                    }
                    return Ok(total_read);
                }
            }

            // Process characters in the buffer
            let start_pos = self.pos;
            let mut found_newline = false;
            let mut newline_pos = self.pos;

            // Look for newline in current buffer
            for i in self.pos..self.cap {
                if self.buffer[i] == b'\n' {
                    found_newline = true;
                    newline_pos = i;
                    break;
                }
            }

            let end_pos = if found_newline {
                newline_pos + 1 // Include the newline
            } else {
                self.cap
            };

            // Convert to string and add to line
            let chunk = std::str::from_utf8(&self.buffer[start_pos..end_pos])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

            line.push_str(chunk);
            total_read += end_pos - start_pos;

            // Check if we should suppress this line
            if !self.suppress_current_line {
                if line.starts_with("<anylinuxfs-") {
                    // This is a control message - suppress everything we've seen so far
                    self.suppress_current_line = true;
                    self.pending_output.clear(); // Discard any pending output
                } else if "<anylinuxfs-".starts_with(line.as_str()) {
                    // Still a potential prefix - buffer this chunk
                    self.pending_output.push_str(chunk);
                } else {
                    // This line can never be a control message - output everything
                    if !self.pending_output.is_empty() {
                        let prefix = self.select_prefix(self.first_chunk);
                        prefix_print!(prefix, "{}", self.pending_output);
                        self.first_chunk = false;
                        self.pending_output.clear();
                    }

                    let prefix = self.select_prefix(self.first_chunk);
                    prefix_print!(prefix, "{}", chunk);
                    self.first_chunk = false;
                    io::stdout().flush().unwrap_or(());
                }
            }

            self.pos = end_pos;

            if found_newline {
                // End of line - output any remaining pending content if not suppressed
                if !self.suppress_current_line && !self.pending_output.is_empty() {
                    let prefix = self.select_prefix(self.first_chunk);
                    prefix_print!(prefix, "{}", self.pending_output);
                    self.first_chunk = false;
                    io::stdout().flush().unwrap_or(());
                }

                // Reset for the next line
                self.suppress_current_line = false;
                self.pending_output.clear();
                self.first_chunk = true;

                // move cursor to the beginning of new line (in case of raw mode)
                _ = safe_print!("\r");

                return Ok(total_read);
            }

            // If buffer is exhausted but no newline found, continue reading
        }
    }
}

use std::sync::OnceLock;

static ORIGINAL_TERMIOS: OnceLock<libc::termios> = OnceLock::new();
static RAW_MODE_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn is_stdin_tty() -> bool {
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

pub fn enable_raw_mode() -> anyhow::Result<()> {
    if RAW_MODE_ENABLED.load(Ordering::Relaxed) {
        return Ok(()); // already enabled
    }

    // Only enable raw mode if stdin is a TTY
    if !is_stdin_tty() {
        // host_println!("Stdin is not a TTY, skipping raw mode");
        return Ok(());
    }

    let fd = libc::STDIN_FILENO;
    // Fetch current attributes
    let mut termios = unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut t) != 0 {
            return Err(io::Error::last_os_error()).context("tcgetattr failed");
        }
        t
    };

    // Store original once (first call)
    let _ = ORIGINAL_TERMIOS.set(termios); // ignore error if already set

    unsafe { libc::cfmakeraw(&mut termios) };
    // Set new attributes
    if unsafe { libc::tcsetattr(fd, libc::TCSADRAIN, &termios) } != 0 {
        return Err(io::Error::last_os_error()).context("tcsetattr failed");
    }

    RAW_MODE_ENABLED.store(true, Ordering::Relaxed);
    // host_println!("Enabled terminal raw mode");
    Ok(())
}

pub fn disable_raw_mode() -> anyhow::Result<()> {
    if !RAW_MODE_ENABLED.load(Ordering::Relaxed) {
        return Ok(()); // already disabled
    }

    // Only restore terminal if stdin is still a TTY
    if !is_stdin_tty() {
        RAW_MODE_ENABLED.store(false, Ordering::Relaxed);
        return Ok(());
    }

    let fd = libc::STDIN_FILENO;
    // Restore original attributes
    if let Some(original) = ORIGINAL_TERMIOS.get() {
        if unsafe { libc::tcsetattr(fd, libc::TCSADRAIN, original) } != 0 {
            return Err(io::Error::last_os_error()).context("tcsetattr failed");
        }
    } else {
        return Err(anyhow!("Original terminal attributes not set"));
    }

    RAW_MODE_ENABLED.store(false, Ordering::Relaxed);
    // host_println!("Disabled terminal raw mode");
    Ok(())
}

pub fn echo_child_output(hnd: &mut Child, log_prefix: Option<log::Prefix>) {
    let out = BufReader::new(hnd.stdout.take().unwrap());
    let err = BufReader::new(hnd.stderr.take().unwrap());

    let thread = thread::spawn(move || {
        for line in err.lines() {
            if let Ok(line) = line {
                prefix_println!(log_prefix, "{}", line);
            }
        }
    });

    for line in out.lines() {
        if let Ok(line) = line {
            prefix_eprintln!(log_prefix, "{}", line);
        }
    }

    thread.join().unwrap();
}

pub struct StdinForwarder {
    thread_hnd: Cell<Option<JoinHandle<anyhow::Result<()>>>>,
    close_tx: mpsc::Sender<()>,
    echo_newline: Arc<AtomicBool>,
}

impl StdinForwarder {
    pub fn new(in_fd: libc::c_int, signals: Subscription<libc::c_int>) -> anyhow::Result<Self> {
        let is_tty = is_stdin_tty();

        if is_tty {
            enable_raw_mode()?;
        }

        _ = thread::spawn(move || {
            for _ in signals {
                host_println!("Termination requested, sending ^C to microVM");
                unsafe {
                    _ = write_to_pipe(in_fd, b"\x03");
                }
                break;
            }
        });

        let echo_newline = Arc::new(AtomicBool::new(false));

        let (close_tx, close_rx) = mpsc::channel();
        let thread_hnd = Cell::new(Some(std::thread::spawn({
            let echo_newline = Arc::clone(&echo_newline);
            move || -> anyhow::Result<()> {
                if is_tty {
                    // TTY mode: use crossterm events for interactive input
                    loop {
                        // `poll()` waits for an `Event` for a given time period
                        if event::poll(Duration::from_millis(50))? {
                            // It's guaranteed that the `read()` won't block when the `poll()`
                            // function returns `true`
                            match event::read()? {
                                Event::Key(event) => {
                                    // _ = safe_print!("DEBUG: {:?}\r\n", event);

                                    if event.is_press() {
                                        if event.modifiers == event::KeyModifiers::empty()
                                            || event.modifiers == event::KeyModifiers::SHIFT
                                        {
                                            match event.code {
                                                event::KeyCode::Enter => unsafe {
                                                    write_to_pipe(in_fd, b"\n")?;
                                                    if echo_newline.load(Ordering::Relaxed) {
                                                        _ = safe_print!("\r\n");
                                                    }
                                                },
                                                event::KeyCode::Backspace => unsafe {
                                                    write_to_pipe(in_fd, b"\x7f")?;
                                                },
                                                event::KeyCode::Char(c) => unsafe {
                                                    write_to_pipe(in_fd, c.to_string().as_bytes())?;
                                                },
                                                _ => (),
                                            }
                                        } else if event.modifiers == event::KeyModifiers::CONTROL {
                                            match event.code {
                                                event::KeyCode::Char('c') => {
                                                    // Send SIGINT (Ctrl+C)
                                                    unsafe {
                                                        write_to_pipe(in_fd, b"\x03")?;
                                                    }
                                                }
                                                event::KeyCode::Char('d') => {
                                                    // Send EOF (Ctrl+D)
                                                    unsafe {
                                                        write_to_pipe(in_fd, b"\x04")?;
                                                    }
                                                }
                                                _ => (),
                                            }
                                        }
                                    }
                                }
                                _ => (),
                            };
                        } else {
                            // Timeout expired and no `Event` is available
                            if let Ok(()) = close_rx.try_recv() {
                                break;
                            }
                        }
                    }
                } else {
                    // Pipe mode: read from stdin directly
                    use std::io::{BufRead, BufReader, stdin};
                    use std::sync::mpsc;

                    // Create a channel for stdin data
                    let (stdin_tx, stdin_rx) = mpsc::channel();

                    // Spawn a thread to read from stdin
                    thread::spawn(move || {
                        let mut reader = BufReader::new(stdin());
                        let mut line = String::new();

                        loop {
                            line.clear();
                            match reader.read_line(&mut line) {
                                Ok(0) => {
                                    // EOF reached
                                    let _ = stdin_tx.send(None);
                                    break;
                                }
                                Ok(_) => {
                                    if stdin_tx.send(Some(line.clone())).is_err() {
                                        break; // Receiver dropped
                                    }
                                }
                                Err(_) => {
                                    let _ = stdin_tx.send(None);
                                    break;
                                }
                            }
                        }
                    });

                    // Main loop to handle both close signals and stdin data
                    loop {
                        // Check for close signal
                        if let Ok(()) = close_rx.try_recv() {
                            break;
                        }

                        // Check for stdin data with timeout
                        match stdin_rx.recv_timeout(Duration::from_millis(50)) {
                            Ok(Some(data)) => unsafe {
                                write_to_pipe(in_fd, data.as_bytes())?;
                            },
                            Ok(None) => {
                                // EOF from stdin
                                unsafe {
                                    write_to_pipe(in_fd, b"\x04")?; // Send EOF to VM
                                }
                                break;
                            }
                            Err(mpsc::RecvTimeoutError::Timeout) => {
                                // No data available, continue loop
                                continue;
                            }
                            Err(mpsc::RecvTimeoutError::Disconnected) => {
                                // Stdin thread terminated
                                break;
                            }
                        }
                    }
                }
                Ok(())
            }
        })));

        Ok(Self {
            thread_hnd,
            close_tx,
            echo_newline,
        })
    }

    pub fn stop(&self) -> anyhow::Result<()> {
        _ = self.close_tx.send(());
        disable_raw_mode()?;
        if let Some(hnd) = self.thread_hnd.take() {
            hnd.join().unwrap()?;
        }
        Ok(())
    }

    pub fn echo_newline(&self, enable: bool) {
        self.echo_newline.store(enable, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_env_vars_simple() {
        // Basic $VAR syntax
        assert_eq!(find_env_vars("$HOME"), HashSet::from(["HOME".into()]));
        assert_eq!(find_env_vars("$PATH"), HashSet::from(["PATH".into()]));
        assert_eq!(
            find_env_vars("$USER_NAME"),
            HashSet::from(["USER_NAME".into()])
        );
    }

    #[test]
    fn test_find_env_vars_braced() {
        // Basic ${VAR} syntax
        assert_eq!(find_env_vars("${HOME}"), HashSet::from(["HOME".into()]));
        assert_eq!(find_env_vars("${PATH}"), HashSet::from(["PATH".into()]));
        assert_eq!(
            find_env_vars("${USER_NAME}"),
            HashSet::from(["USER_NAME".into()])
        );
    }

    #[test]
    fn test_find_env_vars_mixed_syntax() {
        // Mix of both syntaxes
        assert_eq!(
            find_env_vars("$HOME and ${PATH}"),
            HashSet::from(["HOME".into(), "PATH".into()])
        );
        assert_eq!(
            find_env_vars("${USER}:$GROUP"),
            HashSet::from(["USER".into(), "GROUP".into()])
        );
    }

    #[test]
    fn test_find_env_vars_consecutive() {
        // Consecutive variables
        assert_eq!(
            find_env_vars("$A$B"),
            HashSet::from(["A".into(), "B".into()])
        );
        assert_eq!(
            find_env_vars("$HOME$PATH"),
            HashSet::from(["HOME".into(), "PATH".into()])
        );
        assert_eq!(
            find_env_vars("${A}${B}"),
            HashSet::from(["A".into(), "B".into()])
        );
        assert_eq!(
            find_env_vars("$A${B}$C"),
            HashSet::from(["A".into(), "B".into(), "C".into()])
        );
    }

    #[test]
    fn test_find_env_vars_with_text() {
        // Variables mixed with regular text
        assert_eq!(
            find_env_vars("prefix-$VAR-suffix"),
            HashSet::from(["VAR".into()])
        );
        assert_eq!(
            find_env_vars("path/to/$HOME/file"),
            HashSet::from(["HOME".into()])
        );

        assert_eq!(
            find_env_vars("${USER}_config.txt"),
            HashSet::from(["USER".into()])
        );
        assert_eq!(
            find_env_vars("start-$A-middle-$B-end"),
            HashSet::from(["A".into(), "B".into()])
        );
    }

    #[test]
    fn test_find_env_vars_empty_and_edge_cases() {
        // Empty string
        assert_eq!(find_env_vars(""), HashSet::new());

        // No variables
        assert_eq!(find_env_vars("just text"), HashSet::new());

        // Just dollar sign
        assert_eq!(find_env_vars("$"), HashSet::new());

        assert_eq!(find_env_vars("END$"), HashSet::new()); // $ at end with no var name

        // Empty braces
        assert_eq!(find_env_vars("${}"), HashSet::new());

        // Incomplete braced variable
        assert_eq!(find_env_vars("${INCOMPLETE"), HashSet::new());

        // Double dollar signs
        assert_eq!(find_env_vars("$$VAR"), HashSet::from(["VAR".into()]));
    }

    #[test]
    fn test_find_env_vars_invalid_chars() {
        // Variables with invalid characters (should stop at invalid char)
        assert_eq!(find_env_vars("$VAR-suffix"), HashSet::from(["VAR".into()]));
        assert_eq!(find_env_vars("$VAR.ext"), HashSet::from(["VAR".into()]));
        assert_eq!(find_env_vars("$VAR@domain"), HashSet::from(["VAR".into()]));

        // Braced variables should not include invalid chars
        assert_eq!(find_env_vars("${VAR-invalid}"), HashSet::new());
        assert_eq!(find_env_vars("${VAR.invalid}"), HashSet::new());
    }

    #[test]
    fn test_find_env_vars_underscores_and_numbers() {
        // Valid identifier characters
        assert_eq!(find_env_vars("$VAR_123"), HashSet::from(["VAR_123".into()]));
        assert_eq!(
            find_env_vars("$_PRIVATE"),
            HashSet::from(["_PRIVATE".into()])
        );
        assert_eq!(
            find_env_vars("${VAR_123}"),
            HashSet::from(["VAR_123".into()])
        );
        assert_eq!(
            find_env_vars("${_PRIVATE}"),
            HashSet::from(["_PRIVATE".into()])
        );
    }

    #[test]
    fn test_find_env_vars_duplicates() {
        // Duplicate variables should now be deduplicated automatically
        assert_eq!(
            find_env_vars("$HOME and $HOME"),
            HashSet::from(["HOME".into()])
        );
        assert_eq!(
            find_env_vars("${PATH}:$PATH"),
            HashSet::from(["PATH".into()])
        );
    }

    #[test]
    fn test_find_env_vars_complex_expressions() {
        // More complex real-world examples
        assert_eq!(
            find_env_vars("export PATH=$HOME/bin:$PATH"),
            HashSet::from(["HOME".into(), "PATH".into()])
        );
        assert_eq!(
            find_env_vars("${PREFIX}/bin:${HOME}/.local/bin:$PATH"),
            HashSet::from(["PREFIX".into(), "HOME".into(), "PATH".into()])
        );
        assert_eq!(
            find_env_vars("echo \"User: $USER, Home: ${HOME}, Shell: $SHELL\""),
            HashSet::from(["USER".into(), "HOME".into(), "SHELL".into()])
        );
    }

    #[test]
    fn test_find_env_vars_escaped() {
        // Basic escaping
        assert_eq!(find_env_vars("\\$VAR"), HashSet::new());
        assert_eq!(
            find_env_vars("$VAR\\$NOT_VAR"),
            HashSet::from(["VAR".into()])
        );

        // Multiple escaped variables
        assert_eq!(find_env_vars("\\$A \\$B \\$C"), HashSet::new());

        // Mix of escaped and unescaped
        assert_eq!(
            find_env_vars("$REAL \\$FAKE $ANOTHER"),
            HashSet::from(["REAL".into(), "ANOTHER".into()])
        );

        // Escaped braced variables should also work
        assert_eq!(find_env_vars("\\${VAR}"), HashSet::new());
        assert_eq!(
            find_env_vars("${REAL} \\${FAKE}"),
            HashSet::from(["REAL".into()])
        );
    }
}
