use std::{
    error::Error,
    fs::File,
    io::{self, Write},
    os::fd::FromRawFd,
};

use anyhow::Context;

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
}

pub fn fork_with_pty_output() -> anyhow::Result<ForkOutput> {
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
    } else {
        // Parent process

        // Close the slave end of the pty
        let res = unsafe { libc::close(slave_fd) };
        if res < 0 {
            return Err(io::Error::last_os_error()).context("Failed to close slave end of pty");
        }
    }

    Ok(ForkOutput {
        pid,
        pipe_fd: master_fd,
    })
}

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
        })
    }
}

pub fn set_null_stdin() -> anyhow::Result<()> {
    let dev_null_fd = unsafe { libc::open(b"/dev/null\0".as_ptr() as *const i8, libc::O_RDONLY) };
    if dev_null_fd < 0 {
        return Err(io::Error::last_os_error()).context("Failed to open /dev/null");
    }
    let res = unsafe { libc::dup2(dev_null_fd, libc::STDIN_FILENO) };
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
