use std::{
    error::Error,
    fs::File,
    io::{self, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    },
    process::{Command, Stdio},
};

use anyhow::Context;

use crate::Config;

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

pub fn redirect_all_to_file_and_tail_it(
    config: &Config,
    log_file_path: &str,
) -> anyhow::Result<std::process::Child> {
    // Ensure the log file exists
    let log_file = File::create(log_file_path).context("Failed to create log file")?;
    // Spawn the `tail` process
    let mut tail_cmd = Command::new("/usr/bin/tail");
    tail_cmd.arg("-f").arg(log_file_path);

    if let (Some(uid), Some(gid)) = (config.sudo_uid, config.sudo_gid) {
        // run tail with dropped privileges
        tail_cmd.uid(uid).gid(gid);
    }

    let tail_process = tail_cmd.spawn()?;

    // Redirect stdout and stderr to the log file
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

pub fn redirect_all_to_tee(config: &Config, log_file: &str) -> anyhow::Result<std::process::Child> {
    // Spawn the `tee` process
    let mut tee_cmd = Command::new("/usr/bin/tee");

    // Redirect stdin so we can write to it
    tee_cmd.arg(log_file).stdin(Stdio::piped());

    if let (Some(uid), Some(gid)) = (config.sudo_uid, config.sudo_gid) {
        // run tee with dropped privileges
        tee_cmd.uid(uid).gid(gid);
    }

    let tee_process = tee_cmd.spawn()?;

    // Get the stdin of the `tee` process
    let tee_stdin = tee_process.stdin.as_ref().unwrap();

    // Redirect `stdout` to the `tee` process
    let tee_stdin_fd = tee_stdin.as_raw_fd();
    // Redirect `stdout` to the `tee` process
    let res = unsafe { libc::dup2(tee_stdin_fd, libc::STDOUT_FILENO) };
    if res < 0 {
        return Err(io::Error::last_os_error()).context("Failed to redirect stdout to tee process");
    }

    // Redirect `stderr` to the `tee` process
    let res = unsafe { libc::dup2(tee_stdin_fd, libc::STDERR_FILENO) };
    if res < 0 {
        return Err(io::Error::last_os_error()).context("Failed to redirect stderr to tee process");
    }

    // Return the `tee` process handle so the caller can manage it
    Ok(tee_process)
}
