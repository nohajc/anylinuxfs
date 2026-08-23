use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
use common_utils::TELNET_AUTH_DOMAIN;
use ed25519_dalek::{Signature, VerifyingKey};
use rand::RngCore;
use serde::Deserialize;
use std::{
    io::{self, Read, Write},
    os::fd::RawFd,
    os::unix::process::CommandExt,
    process::Command,
    time::{Duration, Instant},
};

const PREFIX: &str = "\x1eALFS-TELNET/1";
const MAX_REQUEST: usize = 64 * 1024;
const AUTH_CHALLENGE_LEN: usize = 32;
const AUTH_SIGNATURE_LEN: usize = 64;
const AUTH_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_AUTH_LINE: usize = 256;

fn is_line_terminator(byte: u8) -> bool {
    matches!(byte, b'\r' | b'\n')
}

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
enum Request {
    Shell { version: u8 },
    Exec { version: u8, argv: Vec<String> },
}

pub fn run() -> Result<()> {
    let saved = attrs().context("read Telnet PTY settings")?;
    prepare_auth_terminal()?;
    let verifying_key = telnet_public_key()?;
    let challenge = random_challenge()?;
    control(&format!(
        "AUTH-CHALLENGE {}",
        STANDARD_NO_PAD.encode(challenge)
    ))?;
    if let Err(error) = authenticate(&verifying_key, &challenge) {
        _ = control(auth_failure_code(&error));
        return Err(error);
    }
    control("READY")?;
    let request = request()?;
    restore(&saved)?;
    claim_controlling_terminal()?;

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
        let mut command = match request {
            Request::Shell { .. } => {
                let mut command = shell_command();
                set_terminal_type(&mut command);
                command
            }
            Request::Exec { argv, .. } => {
                let mut command = Command::new(&argv[0]);
                command.args(&argv[1..]);
                set_terminal_type(&mut command);
                command
            }
        };
        let err = command.exec();
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
    disable_echo()?;
    control(&format!("EXIT {code}"))?;
    // telnetd only knows that bytes reached its PTY, not that they reached the
    // network. Keep its login process alive until the client confirms receipt.
    wait_for_ack();
    unsafe { libc::signal(libc::SIGINT, old_sigint) };
    unsafe { libc::signal(libc::SIGTTOU, old_sigttou) };
    Ok(())
}

fn telnet_public_key() -> Result<VerifyingKey> {
    let encoded = std::env::var("ALFS_TELNET_PUBLIC_KEY").context("Missing Telnet public key")?;
    let bytes: [u8; AUTH_CHALLENGE_LEN] = STANDARD_NO_PAD
        .decode(encoded)
        .context("Decode Telnet public key")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid Telnet public key length"))?;
    VerifyingKey::from_bytes(&bytes).context("Invalid Telnet public key")
}

fn random_challenge() -> Result<[u8; AUTH_CHALLENGE_LEN]> {
    let mut challenge = [0_u8; AUTH_CHALLENGE_LEN];
    rand::rng().fill_bytes(&mut challenge);
    Ok(challenge)
}

fn authenticate(verifying_key: &VerifyingKey, challenge: &[u8; AUTH_CHALLENGE_LEN]) -> Result<()> {
    let line = read_auth_line().context("AUTH-READ")?;
    let signature = parse_auth_response(&line).context("AUTH-PARSE")?;
    verifying_key
        .verify_strict(&auth_message(challenge), &signature)
        .context("AUTH-VERIFY: Invalid Telnet authentication signature")
}

fn auth_failure_code(error: &anyhow::Error) -> &'static str {
    if error.chain().any(|cause| cause.to_string() == "AUTH-READ") {
        "ERROR AUTH-READ"
    } else if error.chain().any(|cause| cause.to_string() == "AUTH-PARSE") {
        "ERROR AUTH-PARSE"
    } else {
        "ERROR AUTH-VERIFY"
    }
}

fn auth_message(challenge: &[u8; AUTH_CHALLENGE_LEN]) -> Vec<u8> {
    let mut message = Vec::with_capacity(TELNET_AUTH_DOMAIN.len() + challenge.len());
    message.extend_from_slice(TELNET_AUTH_DOMAIN);
    message.extend_from_slice(challenge);
    message
}

fn read_auth_line() -> Result<Vec<u8>> {
    read_auth_line_from_fd(libc::STDIN_FILENO, AUTH_TIMEOUT)
}

fn read_auth_line_from_fd(fd: RawFd, timeout: Duration) -> Result<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    let mut line = Vec::new();

    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let mut poll_fd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout = remaining.as_millis().min(i32::MAX as u128) as i32;
        let ready = unsafe { libc::poll(&mut poll_fd, 1, timeout) };
        if ready < 0 {
            return Err(io::Error::last_os_error()).context("Wait for Telnet authentication");
        }
        if ready == 0 {
            break;
        }

        // Do not use `StdinLock` here. It buffers more data than a one-byte
        // `read_exact` returns; polling fd 0 again would then wait even though
        // the rest of the response is already in Rust's userspace buffer.
        let mut buffer = [0_u8; MAX_AUTH_LINE + 1];
        let count = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted
                || error.kind() == io::ErrorKind::WouldBlock
            {
                continue;
            }
            return Err(error).context("Read Telnet authentication response");
        }
        if count == 0 {
            anyhow::bail!("Telnet connection closed during authentication");
        }

        for (index, &byte) in buffer[..count as usize].iter().enumerate() {
            line.push(byte);
            if line.len() > MAX_AUTH_LINE {
                anyhow::bail!("Telnet authentication response is too large");
            }
            if is_line_terminator(byte) {
                if index + 1 != count as usize {
                    anyhow::bail!("Extra data before Telnet authentication completed");
                }
                return Ok(line);
            }
        }
    }
    anyhow::bail!("Timed out waiting for Telnet authentication response")
}

fn parse_auth_response(line: &[u8]) -> Result<Signature> {
    let line = std::str::from_utf8(line)
        .context("Telnet authentication response is not UTF-8")?
        .trim_end_matches(['\r', '\n']);
    let encoded = line
        .strip_prefix("\x1eALFS-TELNET/1 AUTH ")
        .context("Invalid Telnet authentication response")?;
    if encoded.is_empty() || encoded.contains(char::is_whitespace) {
        anyhow::bail!("Invalid Telnet authentication signature encoding");
    }
    let bytes: [u8; AUTH_SIGNATURE_LEN] = STANDARD_NO_PAD
        .decode(encoded)
        .context("Decode Telnet authentication signature")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid Telnet authentication signature length"))?;
    Ok(Signature::from_bytes(&bytes))
}

#[cfg(target_os = "linux")]
fn shell_command() -> Command {
    let mut command = Command::new("/bin/bash");
    command.arg("-l");
    command
}

#[cfg(target_os = "freebsd")]
fn set_terminal_type(command: &mut Command) {
    command.env("TERM", "xterm");
}

#[cfg(not(target_os = "freebsd"))]
fn set_terminal_type(_: &mut Command) {}

#[cfg(target_os = "freebsd")]
fn claim_controlling_terminal() -> Result<()> {
    // BusyBox telnetd creates this session and opens the PTY slave before
    // execing us.  Linux assigns that slave as the controlling terminal on
    // open(2); FreeBSD intentionally does not, so claim it explicitly before
    // Bash tries to enable job control.
    if unsafe { libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY, 0) } < 0 {
        return Err(io::Error::last_os_error()).context("Make Telnet PTY the controlling terminal");
    }
    if unsafe { libc::tcsetpgrp(libc::STDIN_FILENO, libc::getpgrp()) } < 0 {
        return Err(io::Error::last_os_error())
            .context("Make Telnet session process group foreground");
    }
    Ok(())
}

#[cfg(not(target_os = "freebsd"))]
fn claim_controlling_terminal() -> Result<()> {
    Ok(())
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
    let mut stdin = io::stdin().lock();
    let mut line = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        stdin.read_exact(&mut byte)?;
        if is_line_terminator(byte[0]) {
            break;
        }
        line.push(byte[0]);
        if line.len() > MAX_REQUEST {
            anyhow::bail!("Invalid or oversized Telnet startup request");
        }
    }
    if line.is_empty() {
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
    use ed25519_dalek::{Signer, SigningKey};
    use std::{os::fd::AsRawFd, os::unix::net::UnixStream};

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

    #[test]
    fn authentication_signature_is_bound_to_its_challenge() {
        let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        let challenge = [9_u8; AUTH_CHALLENGE_LEN];
        let signature = signing_key.sign(&auth_message(&challenge));
        let line = format!(
            "{PREFIX} AUTH {}\r\n",
            STANDARD_NO_PAD.encode(signature.to_bytes())
        );

        let parsed = parse_auth_response(line.as_bytes()).unwrap();
        assert!(
            signing_key
                .verifying_key()
                .verify_strict(&auth_message(&challenge), &parsed)
                .is_ok()
        );
        assert!(
            signing_key
                .verifying_key()
                .verify_strict(&auth_message(&[8_u8; AUTH_CHALLENGE_LEN]), &parsed)
                .is_err()
        );
    }

    #[test]
    fn authentication_response_rejects_malformed_data() {
        assert!(parse_auth_response(b"\x1eALFS-TELNET/1 AUTH not-base64!\r\n").is_err());
        assert!(parse_auth_response(b"\x1eALFS-TELNET/1 AUTH AA\r\n").is_err());
    }

    #[test]
    fn authentication_read_consumes_the_complete_unbuffered_line() {
        let (mut writer, reader) = UnixStream::pair().unwrap();
        let response = b"\x1eALFS-TELNET/1 AUTH signature\r";
        writer.write_all(response).unwrap();

        assert_eq!(
            read_auth_line_from_fd(reader.as_raw_fd(), Duration::from_secs(1)).unwrap(),
            response
        );
    }

    #[test]
    fn authentication_read_times_out_without_input() {
        let (_writer, reader) = UnixStream::pair().unwrap();
        let error = read_auth_line_from_fd(reader.as_raw_fd(), Duration::from_millis(5))
            .unwrap_err()
            .to_string();

        assert!(error.contains("Timed out"));
    }

    #[test]
    fn authentication_failure_reports_the_failing_stage() {
        let read = anyhow::anyhow!("timed out").context("AUTH-READ");
        let parse = anyhow::anyhow!("malformed").context("AUTH-PARSE");
        let verify = anyhow::anyhow!("invalid signature");

        assert_eq!(auth_failure_code(&read), "ERROR AUTH-READ");
        assert_eq!(auth_failure_code(&parse), "ERROR AUTH-PARSE");
        assert_eq!(auth_failure_code(&verify), "ERROR AUTH-VERIFY");
    }

    #[test]
    fn telnet_lines_accept_busybox_cr_termination() {
        assert!(is_line_terminator(b'\r'));
        assert!(is_line_terminator(b'\n'));
        assert!(!is_line_terminator(b' '));
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
fn prepare_auth_terminal() -> io::Result<()> {
    let mut value = attrs()?;
    unsafe { libc::cfmakeraw(&mut value) };
    restore(&value)
}

fn disable_echo() -> io::Result<()> {
    let mut value = attrs()?;
    value.c_lflag &= !libc::ECHO;
    restore(&value)
}
