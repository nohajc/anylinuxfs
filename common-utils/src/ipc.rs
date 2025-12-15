use std::io::{Read, Write};

use anyhow::{Context, anyhow};
use serde::{Serialize, de::DeserializeOwned};

const MAX_MSG_SIZE: usize = 1048576; // 1 MiB

pub struct Handler {}

impl Handler {
    pub fn read_request<R, S>(stream: &mut S) -> anyhow::Result<R>
    where
        S: Read + Write,
        R: DeserializeOwned,
    {
        let mut size_buf = [0u8; 4];
        stream
            .read_exact(&mut size_buf)
            .context("Failed to read request size")?;
        let size = u32::from_be_bytes(size_buf) as usize;
        if size == 0 {
            return Err(anyhow!("Request size is zero"));
        }
        if size > MAX_MSG_SIZE {
            return Err(anyhow!("Request size is too large"));
        }

        let mut payload_buf = vec![0u8; size];
        stream
            .read_exact(&mut payload_buf)
            .context("Failed to read request payload")?;

        let request = ron::de::from_bytes(&payload_buf).context("Failed to parse request")?;
        Ok(request)
    }

    pub fn write_response<R, S>(stream: &mut S, response: &R) -> anyhow::Result<()>
    where
        S: Read + Write,
        R: ?Sized + Serialize,
    {
        let response_str =
            ron::ser::to_string(&response).context("Failed to serialize response")?;
        let size = response_str.len() as u32;
        let size_buf = size.to_be_bytes();
        stream
            .write_all(&size_buf)
            .context("Failed to write response size")?;
        stream
            .write_all(response_str.as_bytes())
            .context("Failed to write response payload")?;

        Ok(())
    }
}

pub struct Client {}

impl Client {
    pub fn write_request<R, S>(stream: &mut S, request: &R) -> anyhow::Result<()>
    where
        S: Read + Write,
        R: ?Sized + Serialize,
    {
        let request_str = ron::ser::to_string(&request).context("Failed to serialize request")?;
        let size = request_str.len() as u32;
        let size_buf = size.to_be_bytes();
        stream
            .write_all(&size_buf)
            .context("Failed to write request size")?;
        stream
            .write_all(request_str.as_bytes())
            .context("Failed to write request payload")?;

        Ok(())
    }

    pub fn read_response<R, S>(stream: &mut S) -> anyhow::Result<R>
    where
        S: Read + Write,
        R: DeserializeOwned,
    {
        let mut size_buf = [0u8; 4];
        stream
            .read_exact(&mut size_buf)
            .context("Failed to read response size")?;
        let size = u32::from_be_bytes(size_buf) as usize;
        if size == 0 {
            return Err(anyhow!("Response size is zero"));
        }
        if size > MAX_MSG_SIZE {
            return Err(anyhow!("Response size is too large"));
        }

        let mut payload_buf = vec![0u8; size];
        stream
            .read_exact(&mut payload_buf)
            .context("Failed to read response payload")?;

        let response = ron::de::from_bytes(&payload_buf).context("Failed to parse response")?;
        Ok(response)
    }
}
