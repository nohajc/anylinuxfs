use std::{
    net::Ipv4Addr,
    os::fd::AsRawFd,
    os::unix::fs::PermissionsExt,
    os::unix::net::{UnixListener, UnixStream},
    path::Path,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use anyhow::Context;
use common_utils::{
    TELNET_AUTH_DOMAIN, host_eprintln,
    ipc::{Client, Handler},
};
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};

use crate::{devinfo::DevInfo, privilege, settings::MountConfig};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RuntimeInfo {
    pub mount_config: MountConfig,
    pub dev_info: DevInfo,
    pub session_pgid: libc::pid_t,
    pub vmm_pid: libc::pid_t,
    pub net_helper_pid: libc::pid_t,
    pub vm_host: Vec<u8>,
    pub vm_native_ip: Option<Ipv4Addr>,
    pub mount_point: Option<String>,
}

pub struct RuntimeApi {
    pub runtime_info: Arc<Mutex<RuntimeInfo>>,
    signing_key: SigningKey,
    invoker_uid: libc::uid_t,
}

impl RuntimeApi {
    pub fn new(
        runtime_info: Arc<Mutex<RuntimeInfo>>,
        signing_key: SigningKey,
        invoker_uid: libc::uid_t,
    ) -> Self {
        Self {
            runtime_info,
            signing_key,
            invoker_uid,
        }
    }
}

pub fn serve_info(api: RuntimeApi, socket_path: String) {
    _ = thread::spawn(move || {
        let path = Path::new(&socket_path);
        if let Err(e) = UnixHandler::serve(api, path) {
            host_eprintln!("Error in serve_config: {}", e);
        }
    });
}

#[derive(Clone, Deserialize, Serialize)]
pub enum Request {
    GetConfig,
    SignTelnetChallenge(Vec<u8>),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Response {
    Config(RuntimeInfo),
    TelnetSignature(Vec<u8>),
}

struct UnixHandler {}

impl UnixHandler {
    fn serve(api: RuntimeApi, socket_path: &Path) -> anyhow::Result<()> {
        let listener = UnixListener::bind(socket_path).context("Failed to bind to Unix socket")?;

        {
            let rt_info = api.runtime_info.lock().unwrap();
            privilege::chown_to_invoker(
                socket_path,
                rt_info.mount_config.common.privilege.invoker_uid,
                rt_info.mount_config.common.privilege.invoker_gid,
            )?;
        }
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))
            .context("Failed to restrict Unix socket permissions")?;

        for stream in listener.incoming() {
            let Ok(stream) = stream else {
                continue;
            };
            _ = UnixHandler::serve_to_client(stream, &api);
        }

        Ok(())
    }

    fn serve_to_client(mut stream: UnixStream, api: &RuntimeApi) -> anyhow::Result<()> {
        if peer_uid(&stream)? != api.invoker_uid {
            anyhow::bail!("Unix socket client is not the invoking user");
        }
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let req = Handler::read_request(&mut stream)?;
        let resp = match req {
            Request::GetConfig => Response::Config(api.runtime_info.lock().unwrap().clone()),
            Request::SignTelnetChallenge(challenge) => {
                Response::TelnetSignature(sign_telnet_challenge(&api.signing_key, &challenge)?)
            }
        };
        Handler::write_response(&mut stream, &resp)?;

        Ok(())
    }
}

fn sign_telnet_challenge(signing_key: &SigningKey, challenge: &[u8]) -> anyhow::Result<Vec<u8>> {
    let challenge: [u8; 32] = challenge
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid Telnet challenge length"))?;
    let mut message = Vec::with_capacity(TELNET_AUTH_DOMAIN.len() + challenge.len());
    message.extend_from_slice(TELNET_AUTH_DOMAIN);
    message.extend_from_slice(&challenge);
    Ok(signing_key.sign(&message).to_bytes().to_vec())
}

pub struct UnixClient {}

impl UnixClient {
    pub fn make_request(socket_path: &Path, req: Request) -> anyhow::Result<Response> {
        let mut stream = UnixStream::connect(socket_path).context("Failed to connect to socket")?;
        Client::write_request(&mut stream, &req)?;
        let resp = Client::read_response(&mut stream)?;
        Ok(resp)
    }

    pub fn sign_telnet_challenge(socket_path: &Path, challenge: &[u8]) -> anyhow::Result<Vec<u8>> {
        match Self::make_request(
            socket_path,
            Request::SignTelnetChallenge(challenge.to_vec()),
        )? {
            Response::TelnetSignature(signature) => Ok(signature),
            Response::Config(_) => {
                anyhow::bail!("Unexpected runtime API response while signing Telnet challenge")
            }
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn peer_uid(stream: &UnixStream) -> anyhow::Result<libc::uid_t> {
    let mut uid = 0;
    let mut gid = 0;
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return Err(std::io::Error::last_os_error()).context("Get Unix socket peer credentials");
    }
    Ok(uid)
}

#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> anyhow::Result<libc::uid_t> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("Get Unix socket peer credentials");
    }
    Ok(cred.uid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signature, Verifier};

    #[test]
    fn telnet_challenge_signature_is_domain_separated() {
        let signing_key = SigningKey::from_bytes(&[3_u8; 32]);
        let challenge = [4_u8; 32];
        let signature = Signature::from_bytes(
            &sign_telnet_challenge(&signing_key, &challenge)
                .unwrap()
                .try_into()
                .unwrap(),
        );
        let mut message = TELNET_AUTH_DOMAIN.to_vec();
        message.extend_from_slice(&challenge);
        assert!(
            signing_key
                .verifying_key()
                .verify(&message, &signature)
                .is_ok()
        );
        assert!(
            signing_key
                .verifying_key()
                .verify(&challenge, &signature)
                .is_err()
        );
    }

    #[test]
    fn telnet_challenge_requires_32_bytes() {
        let signing_key = SigningKey::from_bytes(&[3_u8; 32]);
        assert!(sign_telnet_challenge(&signing_key, &[0_u8; 31]).is_err());
    }
}
