use std::{
    error::Error,
    fs::File,
    io::{self, BufRead, BufReader, Write},
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

pub struct TeeLogger {
    processor_thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl TeeLogger {
    pub fn new(
        sources: &[libc::c_int],
        process_line: impl Fn(usize, &str) + Send + 'static,
    ) -> Self {
        let mut source_read_fds = Vec::new();

        for &fd in sources {
            let mut pipe_fds: [libc::c_int; 2] = [0; 2];
            let res = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
            if res < 0 {
                panic!("Failed to create pipe: {}", io::Error::last_os_error());
            }

            let read_fd = pipe_fds[0];
            let write_fd = pipe_fds[1];

            // Redirect the source fd to the pipe's write end
            let res = unsafe { libc::dup2(write_fd, fd) };
            if res < 0 {
                panic!(
                    "Failed to redirect fd {} to pipe's write end: {}",
                    fd,
                    io::Error::last_os_error()
                );
            }

            // Close the write end of the pipe
            let res = unsafe { libc::close(write_fd) };
            if res < 0 {
                panic!(
                    "Failed to close write end of pipe: {}",
                    io::Error::last_os_error()
                );
            }

            source_read_fds.push(read_fd);
        }

        let processor_thread_handle = Some(std::thread::spawn(move || {
            let mut fds: libc::fd_set = unsafe { std::mem::zeroed() };

            let mut readers: Vec<BufReader<File>> = source_read_fds
                .iter()
                .map(|&fd| {
                    let file = unsafe { File::from_raw_fd(fd) };
                    BufReader::new(file)
                })
                .collect();

            loop {
                unsafe {
                    libc::FD_ZERO(&mut fds);
                }

                let mut max_fd = 0;
                for &fd in &source_read_fds {
                    unsafe {
                        libc::FD_SET(fd, &mut fds);
                    }
                    if fd > max_fd {
                        max_fd = fd;
                    }
                }

                let res = unsafe {
                    libc::select(
                        max_fd + 1,
                        &mut fds,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                };
                if res < 0 {
                    panic!("select failed: {}", io::Error::last_os_error());
                }

                for (i, reader) in readers.iter_mut().enumerate() {
                    if unsafe { libc::FD_ISSET(source_read_fds[i], &mut fds) } {
                        let mut line = String::new();
                        while let Ok(bytes_read) = reader.read_line(&mut line) {
                            if bytes_read == 0 {
                                // EOF
                                break;
                            }
                            process_line(i, line.trim_end());
                            line.clear();
                        }
                    }
                }
            }
        }));

        TeeLogger {
            processor_thread_handle,
        }
    }
}

impl Drop for TeeLogger {
    fn drop(&mut self) {
        // Wait for the processor thread to finish
        let hnd = self.processor_thread_handle.take();
        if let Err(e) = hnd.unwrap().join() {
            eprintln!("Error joining processor thread: {:?}", e);
        }
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

// TODO: find out how to stop printing to console after console detach
pub fn redirect_all_to_tee(config: &Config, log_file: &str) -> io::Result<std::process::Child> {
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
    unsafe {
        libc::dup2(tee_stdin_fd, libc::STDOUT_FILENO);
    }

    // Redirect `stderr` to the `tee` process
    unsafe {
        libc::dup2(tee_stdin_fd, libc::STDERR_FILENO);
    }

    // Return the `tee` process handle so the caller can manage it
    Ok(tee_process)
}
