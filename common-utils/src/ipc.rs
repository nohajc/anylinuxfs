use std::io::{Read, Write};

use anyhow::Context;
use serde::{Serialize, de::DeserializeOwned};

const MAX_MSG_SIZE: usize = 1048576; // 1 MiB

fn validate_msg_size(size: usize, direction: &str) -> anyhow::Result<()> {
    if size == 0 {
        anyhow::bail!("{} size is zero", direction);
    }
    if size > MAX_MSG_SIZE {
        anyhow::bail!("{} size is too large", direction);
    }
    Ok(())
}

fn read_msg<R, S>(stream: &mut S, direction: &str) -> anyhow::Result<R>
where
    R: DeserializeOwned,
    S: Read + Write,
{
    let mut size_buf = [0u8; 4];
    stream
        .read_exact(&mut size_buf)
        .with_context(|| format!("Failed to read {} size", direction))?;
    let size = u32::from_be_bytes(size_buf) as usize;
    validate_msg_size(size, direction)?;

    let mut payload_buf = vec![0u8; size];
    stream
        .read_exact(&mut payload_buf)
        .with_context(|| format!("Failed to read {} payload", direction))?;

    let msg = ron::de::from_bytes(&payload_buf)
        .with_context(|| format!("Failed to parse {}", direction))?;
    Ok(msg)
}

fn write_msg<R, S>(stream: &mut S, msg: &R, direction: &str) -> anyhow::Result<()>
where
    R: ?Sized + Serialize,
    S: Read + Write,
{
    let msg_str =
        ron::ser::to_string(msg).with_context(|| format!("Failed to serialize {}", direction))?;
    let size = msg_str.len() as u32;
    let size_buf = size.to_be_bytes();
    stream
        .write_all(&size_buf)
        .with_context(|| format!("Failed to write {} size", direction))?;
    stream
        .write_all(msg_str.as_bytes())
        .with_context(|| format!("Failed to write {} payload", direction))?;

    Ok(())
}

pub struct Handler {}

impl Handler {
    pub fn read_request<R, S>(stream: &mut S) -> anyhow::Result<R>
    where
        R: DeserializeOwned,
        S: Read + Write,
    {
        read_msg(stream, "request")
    }

    pub fn write_response<R, S>(stream: &mut S, response: &R) -> anyhow::Result<()>
    where
        R: ?Sized + Serialize,
        S: Read + Write,
    {
        write_msg(stream, response, "response")
    }
}

pub struct Client {}

impl Client {
    pub fn write_request<R, S>(stream: &mut S, request: &R) -> anyhow::Result<()>
    where
        R: ?Sized + Serialize,
        S: Read + Write,
    {
        write_msg(stream, request, "request")
    }

    pub fn read_response<R, S>(stream: &mut S) -> anyhow::Result<R>
    where
        R: DeserializeOwned,
        S: Read + Write,
    {
        read_msg(stream, "response")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_validate_msg_size_zero() {
        let err = validate_msg_size(0, "request").unwrap_err();
        assert!(err.to_string().contains("request size is zero"));
    }

    #[test]
    fn test_validate_msg_size_too_large() {
        let err = validate_msg_size(MAX_MSG_SIZE + 1, "response").unwrap_err();
        assert!(err.to_string().contains("response size is too large"));
    }

    #[test]
    fn test_validate_msg_size_valid() {
        assert!(validate_msg_size(1, "request").is_ok());
        assert!(validate_msg_size(MAX_MSG_SIZE, "response").is_ok());
    }

    #[test]
    fn test_roundtrip_handler_client() {
        // Client writes a request, Handler reads it
        let mut buf = Cursor::new(Vec::new());
        Client::write_request(&mut buf, &"hello").unwrap();

        buf.set_position(0);
        let msg: String = Handler::read_request(&mut buf).unwrap();
        assert_eq!(msg, "hello");
    }

    #[test]
    fn test_roundtrip_handler_response() {
        // Handler writes a response, Client reads it
        let mut buf = Cursor::new(Vec::new());
        Handler::write_response(&mut buf, &42u32).unwrap();

        buf.set_position(0);
        let msg: u32 = Client::read_response(&mut buf).unwrap();
        assert_eq!(msg, 42);
    }
}
