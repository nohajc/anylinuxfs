use std::{
    error::Error,
    fs::{File, Permissions},
    io::{self, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::PermissionsExt, process::CommandExt},
    },
    path::Path,
    process::Command,
};

use anyhow::Context;

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

pub struct ForkOutput {
    pub pid: libc::pid_t,
    pub pipe_fd: libc::c_int,
    redirect_action: Option<Box<dyn FnOnce() -> anyhow::Result<()>>>,
}

impl ForkOutput {
    pub fn redirect(&mut self) -> anyhow::Result<()> {
        if let Some(redirect_fn) = self.redirect_action.take() {
            redirect_fn()?;
        }
        Ok(())
    }
}

#[allow(unused)]
pub enum OutputAction {
    RedirectNow,
    RedirectLater,
}

pub fn fork_with_pty_output(out_action: OutputAction) -> anyhow::Result<ForkOutput> {
    let mut master_fd: libc::c_int = 0;
    let mut slave_fd: libc::c_int = 0;

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
            pipe_fd: slave_fd,
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
            pipe_fd: master_fd,
            redirect_action: None,
        })
    }
}

#[allow(unused)]
pub fn fork_with_piped_output() -> anyhow::Result<ForkOutput> {
    let mut child_output_fds: [libc::c_int; 2] = [0; 2];
    let res = unsafe { libc::pipe(child_output_fds.as_mut_ptr()) };
    if res < 0 {
        return Err(io::Error::last_os_error()).context("Failed to create pipe");
    }

    let child_read_fd = child_output_fds[0];
    let child_write_fd = child_output_fds[1];

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error()).context("Failed to fork process");
    } else if pid == 0 {
        // Child process

        // Close the read end of the pipe
        let res = unsafe { libc::close(child_read_fd) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to close read end of pipe");
        }

        // Redirect stdout and stderr to the write end of the pipe
        let res = unsafe { libc::dup2(child_write_fd, libc::STDOUT_FILENO) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to redirect stdout");
        }
        let res = unsafe { libc::dup2(child_write_fd, libc::STDERR_FILENO) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to redirect stderr");
        }
        // Close the write end of the pipe
        let res = unsafe { libc::close(child_write_fd) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to close write end of pipe");
        }
    } else {
        // Parent process

        // Close the write end of the pipe
        let res = unsafe { libc::close(child_write_fd) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to close write end of pipe");
        }
    }

    Ok(ForkOutput {
        pid,
        pipe_fd: child_read_fd,
        redirect_action: None,
    })
}

// pipe_fd contains the read end of the pipe in the parent
// and the write end of the pipe in the child process
pub fn fork_with_comm_pipe() -> anyhow::Result<ForkOutput> {
    let mut comm_pipe_fds: [libc::c_int; 2] = [0; 2];
    let res = unsafe { libc::pipe(comm_pipe_fds.as_mut_ptr()) };
    if res < 0 {
        return Err(io::Error::last_os_error()).context("Failed to create communication pipe");
    }

    let parent_read_fd = comm_pipe_fds[0];
    let child_write_fd = comm_pipe_fds[1];

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
            pipe_fd: child_write_fd,
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
            pipe_fd: parent_read_fd,
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
        return Err(io::Error::last_os_error()).context("Failed to redirect stdin to /dev/null");
    }
    let res = unsafe { libc::close(dev_null_fd) };
    if res < 0 {
        return Err(io::Error::last_os_error()).context("Failed to close /dev/null fd");
    }

    Ok(())
}

pub unsafe fn write_to_pipe(pipe_fd: libc::c_int, data: &[u8]) -> anyhow::Result<()> {
    unsafe { File::from_raw_fd(pipe_fd) }
        .write_all(data)
        .context("Failed to write to pipe")?;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ActionID(usize);

pub struct Deferred<'a> {
    actions: Vec<(ActionID, Box<dyn FnOnce() + 'a>)>,
    last_id: ActionID,
}

impl<'a> Deferred<'a> {
    pub fn new() -> Self {
        Self {
            actions: Vec::new(),
            last_id: ActionID(0),
        }
    }

    pub fn add<'b, F>(&mut self, action: F) -> ActionID
    where
        F: FnOnce() + 'b,
        'b: 'a,
    {
        let id = self.last_id;
        self.actions.push((id, Box::new(action)));
        self.last_id.0 += 1;
        id
    }

    #[allow(unused)]
    pub fn call_now(&mut self, id: ActionID) {
        if let Some((_, action)) = self.pop_action(id) {
            action();
        }
    }

    fn pop_action(&mut self, id: ActionID) -> Option<(ActionID, Box<dyn FnOnce() + 'a>)> {
        self.actions
            .iter()
            .position(|(i, _)| *i == id)
            .map(|idx| self.actions.remove(idx))
    }

    pub fn remove(&mut self, id: ActionID) -> bool {
        self.pop_action(id).is_some()
    }

    pub fn remove_all(&mut self) {
        self.actions.clear();
    }
}

impl<'a> Drop for Deferred<'a> {
    fn drop(&mut self) {
        for (_id, action) in self.actions.drain(..).rev() {
            action();
        }
    }
}
