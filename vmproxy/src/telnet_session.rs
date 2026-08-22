use anyhow::{Context, Result};
use serde::Deserialize;
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::process::CommandExt,
    process::Command,
    time::{Duration, Instant},
};

const PREFIX: &str = "\x1eALFS-TELNET/1";
const MAX_REQUEST: usize = 64 * 1024;

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
enum Request {
    Shell { version: u8 },
    Exec { version: u8, argv: Vec<String> },
}

pub fn run() -> Result<()> {
    let saved = attrs().context("read Telnet PTY settings")?;
    echo(false)?;
    control("READY")?;
    let request = request()?;
    restore(&saved)?;

    // BusyBox has already made this wrapper's process group foreground. Keep
    // the child in that group so it receives terminal signals, while the
    // wrapper ignores them and remains alive to report the child's status.
    let old_sigint = unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) };
    let old_sigttou = unsafe { libc::signal(libc::SIGTTOU, libc::SIG_IGN) };
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        unsafe { libc::signal(libc::SIGINT, old_sigint) };
        unsafe { libc::signal(libc::SIGTTOU, old_sigttou) };
        return Err(io::Error::last_os_error()).context("fork Telnet child");
    }
    if pid == 0 {
        unsafe { libc::signal(libc::SIGINT, libc::SIG_DFL) };
        unsafe { libc::signal(libc::SIGTTOU, libc::SIG_DFL) };
        let err = match request {
            Request::Shell { .. } => shell_command().exec(),
            Request::Exec { argv, .. } => Command::new(&argv[0]).args(&argv[1..]).exec(),
        };
        eprintln!("Failed to start Telnet program: {err}");
        unsafe { libc::_exit(127) };
    }

    let mut status = 0;
    if unsafe { libc::waitpid(pid, &mut status, 0) } < 0 {
        return Err(io::Error::last_os_error()).context("wait for Telnet child");
    }
    let code = if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        libc::WTERMSIG(status) + 128
    } else {
        1
    };
    echo(false)?;
    control(&format!("EXIT {code}"))?;
    // telnetd only knows that bytes reached its PTY, not that they reached the
    // network. Keep its login process alive until the client confirms receipt.
    wait_for_ack();
    unsafe { libc::signal(libc::SIGINT, old_sigint) };
    unsafe { libc::signal(libc::SIGTTOU, old_sigttou) };
    Ok(())
}

#[cfg(target_os = "linux")]
fn shell_command() -> Command {
    let mut command = Command::new("/bin/bash");
    command.arg("-l");
    command
}

#[cfg(target_os = "freebsd")]
fn shell_command() -> Command {
    let mut command = Command::new("/usr/local/bin/bash");
    command.arg("-l");
    command
}

#[cfg(target_os = "macos")]
fn shell_command() -> Command {
    unreachable!("Telnet sessions run only in a guest")
}

fn request() -> Result<Request> {
    let mut line = Vec::new();
    let count = BufReader::new(io::stdin().lock())
        .take((MAX_REQUEST + 1) as u64)
        .read_until(b'\n', &mut line)?;
    if count == 0 || count > MAX_REQUEST || !line.ends_with(b"\n") {
        anyhow::bail!("Invalid or oversized Telnet startup request");
    }
    parse_request(&line)
}

fn parse_request(line: &[u8]) -> Result<Request> {
    let request: Request = serde_json::from_slice(line).context("Invalid Telnet startup JSON")?;
    match &request {
        Request::Shell { version } if *version == 1 => Ok(request),
        Request::Exec { version, argv } if *version == 1 && !argv.is_empty() => Ok(request),
        _ => anyhow::bail!("Unsupported Telnet startup request"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_shell_and_exec_requests() {
        assert!(matches!(
            parse_request(br#"{"version":1,"mode":"shell"}"#),
            Ok(Request::Shell { .. })
        ));
        assert!(matches!(
            parse_request(br#"{"version":1,"mode":"exec","argv":["printf","a b"]}"#),
            Ok(Request::Exec { .. })
        ));
    }

    #[test]
    fn rejects_invalid_startup_requests() {
        assert!(parse_request(br#"{"version":2,"mode":"shell"}"#).is_err());
        assert!(parse_request(br#"{"version":1,"mode":"exec","argv":[]}"#).is_err());
        assert!(parse_request(b"not json").is_err());
    }
}

fn control(message: &str) -> Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(format!("{PREFIX} {message}\r\n").as_bytes())?;
    out.flush()?;
    Ok(())
}

fn wait_for_ack() {
    const ACK: &[u8] = b"\x1eALFS-TELNET/1 ACK";
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut received = Vec::new();
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let mut poll_fd = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout = remaining.as_millis().min(i32::MAX as u128) as i32;
        if unsafe { libc::poll(&mut poll_fd, 1, timeout) } <= 0 {
            return;
        }
        let mut buffer = [0_u8; 128];
        let count = unsafe { libc::read(0, buffer.as_mut_ptr().cast(), buffer.len()) };
        if count <= 0 {
            return;
        }
        received.extend_from_slice(&buffer[..count as usize]);
        if received.windows(ACK.len()).any(|window| window == ACK) {
            return;
        }
        if received.len() > ACK.len() {
            received.drain(..received.len() - ACK.len());
        }
    }
}
fn attrs() -> io::Result<libc::termios> {
    let mut value = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(0, &mut value) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(value)
    }
}
fn restore(value: &libc::termios) -> io::Result<()> {
    if unsafe { libc::tcsetattr(0, libc::TCSANOW, value) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn echo(enabled: bool) -> io::Result<()> {
    let mut value = attrs()?;
    if enabled {
        value.c_lflag |= libc::ECHO;
    } else {
        value.c_lflag &= !libc::ECHO;
    }
    restore(&value)
}
