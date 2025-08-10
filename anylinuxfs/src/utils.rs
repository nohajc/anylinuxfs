use std::{
    collections::HashSet,
    error::Error,
    ffi::CString,
    fs::{File, Permissions},
    io::{self, Read, Write},
    mem::ManuallyDrop,
    net::IpAddr,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::PermissionsExt, process::CommandExt},
    },
    path::Path,
    process::Command,
};

use anyhow::{Context, anyhow};
use common_utils::{log::Prefix, prefix_print};

use crate::MountConfig;

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

pub struct CommFd {
    fd: libc::c_int,
}
pub struct PtyFd {
    fd: libc::c_int,
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
    fn pty_fd(&self) -> libc::c_int;
}

impl HasPtyFd for PtyFd {
    fn pty_fd(&self) -> libc::c_int {
        self.fd
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

pub struct ForkOutput<O, I = ()> {
    pub pid: libc::pid_t,
    pub out_fds: O,
    pub in_fds: I,
    redirect_action: Option<Box<dyn FnOnce() -> anyhow::Result<()>>>,
}

impl<O, I> ForkOutput<O, I> {
    pub fn redirect(&mut self) -> anyhow::Result<()> {
        if let Some(redirect_fn) = self.redirect_action.take() {
            redirect_fn()?;
        }
        Ok(())
    }
}

impl<I> HasCommFd for ForkOutput<CommFd, I> {
    fn comm_fd(&self) -> libc::c_int {
        self.out_fds.comm_fd()
    }
}

impl<I> HasPtyFd for ForkOutput<PtyFd, I> {
    fn pty_fd(&self) -> libc::c_int {
        self.out_fds.pty_fd()
    }
}

impl<I> HasPipeOutFds for ForkOutput<PipeOutFds, I> {
    fn out_fd(&self) -> libc::c_int {
        self.out_fds.out_fd()
    }
    fn err_fd(&self) -> libc::c_int {
        self.out_fds.err_fd()
    }
}

impl<O> HasPipeInFd for ForkOutput<O, PipeInFd> {
    fn in_fd(&self) -> libc::c_int {
        self.in_fds.in_fd()
    }
}

#[allow(unused)]
pub enum OutputAction {
    RedirectNow,
    RedirectLater,
}

pub fn fork_with_pty_output(
    out_action: OutputAction,
) -> anyhow::Result<ForkOutput<PtyFd, PipeInFd>> {
    let mut master_fd: libc::c_int = 0;
    let mut slave_fd: libc::c_int = 0;

    let (child_in_read_fd, child_in_write_fd) = new_pipe()?;

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
        let res = unsafe { libc::close(child_in_write_fd) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to close write end of pipe");
        }

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
            // let res = unsafe { libc::dup2(slave_fd, libc::STDIN_FILENO) };
            // if res < 0 {
            //     return Err(io::Error::last_os_error()).context("Failed to redirect stdin to pty");
            // }

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

            // Close the slave end of the pty
            let res = unsafe { libc::close(slave_fd) };
            if res < 0 {
                return Err(io::Error::last_os_error()).context("Failed to close slave end of pty");
            }

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
            out_fds: PtyFd { fd: slave_fd },
            in_fds: PipeInFd {
                in_fd: child_in_write_fd,
            },
            redirect_action,
        })
    } else {
        // Parent process

        // Close the read end of the pipe
        let res = unsafe { libc::close(child_in_read_fd) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to close read end of pipe");
        }

        // Close the slave end of the pty
        let res = unsafe { libc::close(slave_fd) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to close slave end of pty");
        }

        Ok(ForkOutput {
            pid,
            out_fds: PtyFd { fd: master_fd },
            in_fds: PipeInFd {
                in_fd: child_in_write_fd,
            },
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
        redirect_action: None,
    })
}

// pipe_fd contains the read end of the pipe in the parent
// and the write end of the pipe in the child process
pub fn fork_with_comm_pipe() -> anyhow::Result<ForkOutput<CommFd>> {
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
            out_fds: CommFd { fd: child_write_fd },
            in_fds: (),
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
            out_fds: CommFd { fd: parent_read_fd },
            in_fds: (),
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

#[allow(unused)]
pub fn redirect_all_to_file_and_tail_it(
    config: &MountConfig,
) -> anyhow::Result<Option<std::process::Child>> {
    let mut touch_cmd = Command::new("/usr/bin/touch");
    touch_cmd.arg(&config.common.log_file_path);

    let mut tail_cmd = Command::new("/usr/bin/tail");
    tail_cmd.arg("-f").arg(&config.common.log_file_path);

    if let (Some(uid), Some(gid)) = (config.common.sudo_uid, config.common.sudo_gid) {
        // run touch with dropped privileges
        touch_cmd.uid(uid).gid(gid);

        // run tail with dropped privileges
        tail_cmd.uid(uid).gid(gid);
    }

    touch_cmd.status().context("Failed to touch log file")?;
    let tail_process = match config.verbose {
        false => None,
        true => Some(tail_cmd.spawn()?),
    };

    // Redirect stdout and stderr to the log file
    let log_file =
        File::create(&config.common.log_file_path).context("Failed to create log file")?;
    let log_file_fd = log_file.as_raw_fd();

    let res = unsafe { libc::dup2(log_file_fd, libc::STDOUT_FILENO) };
    if res < 0 {
        return Err(io::Error::last_os_error()).context("Failed to redirect stdout to log file");
    }
    let res = unsafe { libc::dup2(log_file_fd, libc::STDERR_FILENO) };
    if res < 0 {
        return Err(io::Error::last_os_error()).context("Failed to redirect stderr to log file");
    }

    // Return the `tail` process handle so the caller can manage it
    Ok(tail_process)
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

pub fn check_port_availability(ip: impl Into<IpAddr>, port: u16) -> anyhow::Result<()> {
    std::net::TcpListener::bind((ip.into(), port))
        .map(|_| ())
        .map_err(|e| {
            if e.kind() == io::ErrorKind::AddrInUse {
                anyhow!("port {port} already in use")
            } else {
                anyhow!("unexpected error checking port {port}: {e}")
            }
        })
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

pub fn find_env_vars(expression: &str) -> HashSet<String> {
    let mut vars = HashSet::new();
    let mut in_var = false;
    let mut in_braces = false;
    let mut var_name = String::new();

    let mut last_char = '^';
    for c in expression.chars() {
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
                var_name.push(c);
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
                var_name.push(c);
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
}

fn select_prefix(first_chunk: bool) -> Option<Prefix> {
    if first_chunk {
        Some(Prefix::Guest)
    } else {
        None
    }
}

impl<R: Read> PassthroughBufReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            buffer: vec![0; 8192],
            pos: 0,
            cap: 0,
            suppress_current_line: false,
            pending_output: String::new(),
            first_chunk: true,
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
                        let prefix = select_prefix(self.first_chunk);
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
                        let prefix = select_prefix(self.first_chunk);
                        prefix_print!(prefix, "{}", self.pending_output);
                        self.first_chunk = false;
                        self.pending_output.clear();
                    }

                    let prefix = select_prefix(self.first_chunk);
                    prefix_print!(prefix, "{}", chunk);
                    self.first_chunk = false;
                    io::stdout().flush().unwrap_or(());
                }
            }

            self.pos = end_pos;

            if found_newline {
                // End of line - output any remaining pending content if not suppressed
                if !self.suppress_current_line && !self.pending_output.is_empty() {
                    let prefix = select_prefix(self.first_chunk);
                    prefix_print!(prefix, "{}", self.pending_output);
                    self.first_chunk = false;
                    io::stdout().flush().unwrap_or(());
                }

                // Reset for the next line
                self.suppress_current_line = false;
                self.pending_output.clear();
                self.first_chunk = true;
                return Ok(total_read);
            }

            // If buffer is exhausted but no newline found, continue reading
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_env_vars_simple() {
        // Basic $VAR syntax
        assert_eq!(find_env_vars("$HOME"), HashSet::from(["HOME".to_string()]));
        assert_eq!(find_env_vars("$PATH"), HashSet::from(["PATH".to_string()]));
        assert_eq!(
            find_env_vars("$USER_NAME"),
            HashSet::from(["USER_NAME".to_string()])
        );
    }

    #[test]
    fn test_find_env_vars_braced() {
        // Basic ${VAR} syntax
        assert_eq!(
            find_env_vars("${HOME}"),
            HashSet::from(["HOME".to_string()])
        );
        assert_eq!(
            find_env_vars("${PATH}"),
            HashSet::from(["PATH".to_string()])
        );
        assert_eq!(
            find_env_vars("${USER_NAME}"),
            HashSet::from(["USER_NAME".to_string()])
        );
    }

    #[test]
    fn test_find_env_vars_mixed_syntax() {
        // Mix of both syntaxes
        assert_eq!(
            find_env_vars("$HOME and ${PATH}"),
            HashSet::from(["HOME".to_string(), "PATH".to_string()])
        );
        assert_eq!(
            find_env_vars("${USER}:$GROUP"),
            HashSet::from(["USER".to_string(), "GROUP".to_string()])
        );
    }

    #[test]
    fn test_find_env_vars_consecutive() {
        // Consecutive variables
        assert_eq!(
            find_env_vars("$A$B"),
            HashSet::from(["A".to_string(), "B".to_string()])
        );
        assert_eq!(
            find_env_vars("$HOME$PATH"),
            HashSet::from(["HOME".to_string(), "PATH".to_string()])
        );
        assert_eq!(
            find_env_vars("${A}${B}"),
            HashSet::from(["A".to_string(), "B".to_string()])
        );
        assert_eq!(
            find_env_vars("$A${B}$C"),
            HashSet::from(["A".to_string(), "B".to_string(), "C".to_string()])
        );
    }

    #[test]
    fn test_find_env_vars_with_text() {
        // Variables mixed with regular text
        assert_eq!(
            find_env_vars("prefix-$VAR-suffix"),
            HashSet::from(["VAR".to_string()])
        );
        assert_eq!(
            find_env_vars("path/to/$HOME/file"),
            HashSet::from(["HOME".to_string()])
        );
        assert_eq!(
            find_env_vars("${USER}_config.txt"),
            HashSet::from(["USER".to_string()])
        );
        assert_eq!(
            find_env_vars("start-$A-middle-$B-end"),
            HashSet::from(["A".to_string(), "B".to_string()])
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
        assert_eq!(find_env_vars("$$VAR"), HashSet::from(["VAR".to_string()]));
    }

    #[test]
    fn test_find_env_vars_invalid_chars() {
        // Variables with invalid characters (should stop at invalid char)
        assert_eq!(
            find_env_vars("$VAR-suffix"),
            HashSet::from(["VAR".to_string()])
        );
        assert_eq!(
            find_env_vars("$VAR.ext"),
            HashSet::from(["VAR".to_string()])
        );
        assert_eq!(
            find_env_vars("$VAR@domain"),
            HashSet::from(["VAR".to_string()])
        );

        // Braced variables should not include invalid chars
        assert_eq!(find_env_vars("${VAR-invalid}"), HashSet::new());
        assert_eq!(find_env_vars("${VAR.invalid}"), HashSet::new());
    }

    #[test]
    fn test_find_env_vars_underscores_and_numbers() {
        // Valid identifier characters
        assert_eq!(
            find_env_vars("$VAR_123"),
            HashSet::from(["VAR_123".to_string()])
        );
        assert_eq!(
            find_env_vars("$_PRIVATE"),
            HashSet::from(["_PRIVATE".to_string()])
        );
        assert_eq!(
            find_env_vars("${VAR_123}"),
            HashSet::from(["VAR_123".to_string()])
        );
        assert_eq!(
            find_env_vars("${_PRIVATE}"),
            HashSet::from(["_PRIVATE".to_string()])
        );
    }

    #[test]
    fn test_find_env_vars_duplicates() {
        // Duplicate variables should now be deduplicated automatically
        assert_eq!(
            find_env_vars("$HOME and $HOME"),
            HashSet::from(["HOME".to_string()])
        );
        assert_eq!(
            find_env_vars("${PATH}:$PATH"),
            HashSet::from(["PATH".to_string()])
        );
    }

    #[test]
    fn test_find_env_vars_complex_expressions() {
        // More complex real-world examples
        assert_eq!(
            find_env_vars("export PATH=$HOME/bin:$PATH"),
            HashSet::from(["HOME".to_string(), "PATH".to_string()])
        );
        assert_eq!(
            find_env_vars("${PREFIX}/bin:${HOME}/.local/bin:$PATH"),
            HashSet::from(["PREFIX".to_string(), "HOME".to_string(), "PATH".to_string()])
        );
        assert_eq!(
            find_env_vars("echo \"User: $USER, Home: ${HOME}, Shell: $SHELL\""),
            HashSet::from(["USER".to_string(), "HOME".to_string(), "SHELL".to_string()])
        );
    }

    #[test]
    fn test_find_env_vars_escaped() {
        // Basic escaping
        assert_eq!(find_env_vars("\\$VAR"), HashSet::new());
        assert_eq!(
            find_env_vars("$VAR\\$NOT_VAR"),
            HashSet::from(["VAR".to_string()])
        );

        // Multiple escaped variables
        assert_eq!(find_env_vars("\\$A \\$B \\$C"), HashSet::new());

        // Mix of escaped and unescaped
        assert_eq!(
            find_env_vars("$REAL \\$FAKE $ANOTHER"),
            HashSet::from(["REAL".to_string(), "ANOTHER".to_string()])
        );

        // Escaped braced variables should also work
        assert_eq!(find_env_vars("\\${VAR}"), HashSet::new());
        assert_eq!(
            find_env_vars("${REAL} \\${FAKE}"),
            HashSet::from(["REAL".to_string()])
        );
    }
}
