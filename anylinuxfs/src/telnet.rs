//! Small Telnet client used by `anylinuxfs vm`.

use anyhow::{Context, Result};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size};
use serde::Serialize;
use std::{
    env,
    io::{self, Read, Write},
    net::TcpStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;
const OPT_BINARY: u8 = 0;
const OPT_ECHO: u8 = 1;
const OPT_SUPPRESS_GO_AHEAD: u8 = 3;
const OPT_TERMINAL_TYPE: u8 = 24;
const OPT_NAWS: u8 = 31;
const CONTROL_PREFIX: &[u8] = b"\x1eALFS-TELNET/1 ";

#[derive(Serialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub(crate) enum StartupRequest {
    Shell { version: u8 },
    Exec { version: u8, argv: Vec<String> },
}

impl StartupRequest {
    pub(crate) fn shell() -> Self {
        Self::Shell { version: 1 }
    }

    pub(crate) fn exec(argv: Vec<String>) -> Self {
        Self::Exec { version: 1, argv }
    }
}

struct RawMode;

impl RawMode {
    fn enable() -> io::Result<Self> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        _ = disable_raw_mode();
    }
}

/// Runs an interactive session and returns the exit status reported by the
/// guest session wrapper.
pub(crate) fn run(host: &str, port: u16, request: StartupRequest) -> Result<i32> {
    let stream = TcpStream::connect((host, port))
        .with_context(|| format!("Connect to Telnet service at {host}:{port}"))?;
    stream.set_nodelay(true)?;

    let terminal_type = env::var("TERM").unwrap_or_else(|_| "xterm-256color".into());
    let dimensions = size().unwrap_or((80, 24));
    let writer = Arc::new(Mutex::new(stream.try_clone()?));
    let local_echo = Arc::new(AtomicBool::new(true));
    let _raw_mode = RawMode::enable()?;

    receive_loop(
        stream,
        writer,
        local_echo,
        terminal_type,
        dimensions,
        request,
    )
}

#[derive(Debug)]
enum Event {
    Data(Vec<u8>),
    Negotiation(u8, u8),
    Subnegotiation(u8, Vec<u8>),
}

#[derive(Clone, Copy)]
enum ParseState {
    Data,
    Iac,
    Negotiation(u8),
    SubOption,
    SubData,
    SubIac,
}

struct TelnetCodec {
    state: ParseState,
    data: Vec<u8>,
    option: u8,
    sub_data: Vec<u8>,
}

impl Default for TelnetCodec {
    fn default() -> Self {
        Self {
            state: ParseState::Data,
            data: Vec::new(),
            option: 0,
            sub_data: Vec::new(),
        }
    }
}

impl TelnetCodec {
    fn feed(&mut self, input: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        for &byte in input {
            match self.state {
                ParseState::Data if byte == IAC => {
                    self.flush(&mut events);
                    self.state = ParseState::Iac;
                }
                ParseState::Data => self.data.push(byte),
                ParseState::Iac if matches!(byte, WILL | WONT | DO | DONT) => {
                    self.state = ParseState::Negotiation(byte)
                }
                ParseState::Iac if byte == SB => self.state = ParseState::SubOption,
                ParseState::Iac if byte == IAC => {
                    self.data.push(IAC);
                    self.state = ParseState::Data;
                }
                ParseState::Iac => self.state = ParseState::Data,
                ParseState::Negotiation(command) => {
                    events.push(Event::Negotiation(command, byte));
                    self.state = ParseState::Data;
                }
                ParseState::SubOption => {
                    self.option = byte;
                    self.sub_data.clear();
                    self.state = ParseState::SubData;
                }
                ParseState::SubData if byte == IAC => self.state = ParseState::SubIac,
                ParseState::SubData => self.sub_data.push(byte),
                ParseState::SubIac if byte == IAC => {
                    self.sub_data.push(IAC);
                    self.state = ParseState::SubData;
                }
                ParseState::SubIac if byte == SE => {
                    events.push(Event::Subnegotiation(
                        self.option,
                        std::mem::take(&mut self.sub_data),
                    ));
                    self.state = ParseState::Data;
                }
                ParseState::SubIac => self.state = ParseState::Iac,
            }
        }
        self.flush(&mut events);
        events
    }

    fn flush(&mut self, events: &mut Vec<Event>) {
        if !self.data.is_empty() {
            events.push(Event::Data(std::mem::take(&mut self.data)));
        }
    }
}

struct Negotiator {
    terminal_type: Vec<u8>,
    dimensions: (u16, u16),
    local_echo: bool,
}

impl Negotiator {
    fn new(terminal_type: String, dimensions: (u16, u16)) -> Self {
        Self {
            terminal_type: terminal_type.into_bytes(),
            dimensions,
            local_echo: true,
        }
    }

    fn handle(&mut self, event: &Event) -> Vec<Vec<u8>> {
        match event {
            Event::Negotiation(WILL, option) if supports_remote(*option) => {
                if *option == OPT_ECHO {
                    self.local_echo = false;
                }
                vec![negotiation(DO, *option)]
            }
            Event::Negotiation(WILL, option) => vec![negotiation(DONT, *option)],
            Event::Negotiation(WONT, option) => {
                if *option == OPT_ECHO {
                    self.local_echo = true;
                }
                Vec::new()
            }
            Event::Negotiation(DO, option) if supports_local(*option) => {
                let mut messages = vec![negotiation(WILL, *option)];
                if *option == OPT_NAWS {
                    let (cols, rows) = self.dimensions;
                    messages.push(subnegotiation(
                        OPT_NAWS,
                        &[cols.to_be_bytes(), rows.to_be_bytes()].concat(),
                    ));
                }
                messages
            }
            Event::Negotiation(DO, option) => vec![negotiation(WONT, *option)],
            Event::Subnegotiation(OPT_TERMINAL_TYPE, data) if data.first() == Some(&1) => {
                let mut data = vec![0];
                data.extend_from_slice(&self.terminal_type);
                vec![subnegotiation(OPT_TERMINAL_TYPE, &data)]
            }
            _ => Vec::new(),
        }
    }
}

fn supports_remote(option: u8) -> bool {
    matches!(option, OPT_BINARY | OPT_ECHO | OPT_SUPPRESS_GO_AHEAD)
}
fn supports_local(option: u8) -> bool {
    matches!(
        option,
        OPT_BINARY | OPT_SUPPRESS_GO_AHEAD | OPT_TERMINAL_TYPE | OPT_NAWS
    )
}
fn encode_data(data: &[u8]) -> Vec<u8> {
    data.iter()
        .flat_map(|&byte| [byte].into_iter().chain((byte == IAC).then_some(IAC)))
        .collect()
}
fn negotiation(command: u8, option: u8) -> Vec<u8> {
    vec![IAC, command, option]
}
fn subnegotiation(option: u8, data: &[u8]) -> Vec<u8> {
    let mut result = vec![IAC, SB, option];
    result.extend(encode_data(data));
    result.extend([IAC, SE]);
    result
}

/// Keeps an incomplete control line out of the terminal while allowing normal
/// output to be shown as soon as it is known not to be a control record.
struct ControlRecords {
    ready: bool,
    pending: Vec<u8>,
    exit: Option<i32>,
}

impl ControlRecords {
    fn push(&mut self, input: &[u8]) -> Result<Vec<u8>> {
        let mut output = Vec::new();
        for &byte in input {
            self.pending.push(byte);
            if self.pending.len() <= CONTROL_PREFIX.len()
                && self.pending != CONTROL_PREFIX[..self.pending.len()]
            {
                if self.ready {
                    output.append(&mut self.pending);
                } else {
                    self.pending.clear();
                }
                continue;
            }
            if self.pending.len() > 128 || byte == b'\n' {
                let line = std::mem::take(&mut self.pending);
                if let Some(payload) = line.strip_prefix(CONTROL_PREFIX) {
                    let payload = std::str::from_utf8(payload)?.trim();
                    if payload == "READY" {
                        self.ready = true;
                    } else if let Some(code) = payload.strip_prefix("EXIT ") {
                        self.exit = Some(code.parse().context("Invalid Telnet exit status")?);
                    }
                } else {
                    if self.ready {
                        output.extend(line);
                    }
                }
            }
        }
        Ok(output)
    }
}

fn receive_loop(
    mut reader: TcpStream,
    writer: Arc<Mutex<TcpStream>>,
    local_echo: Arc<AtomicBool>,
    terminal_type: String,
    dimensions: (u16, u16),
    request: StartupRequest,
) -> Result<i32> {
    let mut codec = TelnetCodec::default();
    let mut negotiator = Negotiator::new(terminal_type, dimensions);
    let mut controls = ControlRecords {
        ready: false,
        pending: Vec::new(),
        exit: None,
    };
    let mut request = Some(request);
    let mut input_started = false;
    let mut buffer = [0_u8; 4096];

    loop {
        let count = reader.read(&mut buffer).context("Read Telnet data")?;
        if count == 0 {
            return controls
                .exit
                .context("Telnet connection closed without an exit status");
        }
        for event in codec.feed(&buffer[..count]) {
            if let Event::Data(data) = &event {
                let output = controls.push(data)?;
                if !output.is_empty() {
                    let mut stdout = io::stdout().lock();
                    stdout.write_all(&output)?;
                    stdout.flush()?;
                }
                if controls.ready && !input_started {
                    let json = serde_json::to_vec(&request.take().unwrap())?;
                    let mut line = json;
                    line.extend_from_slice(b"\r\n");
                    send(&writer, &encode_data(&line))?;
                    spawn_input_thread(Arc::clone(&writer), Arc::clone(&local_echo));
                    input_started = true;
                }
                if let Some(status) = controls.exit {
                    send(&writer, &encode_data(b"\x1eALFS-TELNET/1 ACK\r\n"))?;
                    return Ok(status);
                }
            }
            for response in negotiator.handle(&event) {
                send(&writer, &response)?;
            }
            local_echo.store(negotiator.local_echo, Ordering::Relaxed);
        }
    }
}

fn spawn_input_thread(writer: Arc<Mutex<TcpStream>>, local_echo: Arc<AtomicBool>) {
    thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut buffer = [0_u8; 512];
        loop {
            let count = match stdin.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(count) => count,
            };
            let input = &buffer[..count];
            if local_echo.load(Ordering::Relaxed) {
                let mut stdout = io::stdout().lock();
                if stdout
                    .write_all(input)
                    .and_then(|_| stdout.flush())
                    .is_err()
                {
                    return;
                }
            }
            if send(&writer, &encode_data(input)).is_err() {
                return;
            }
        }
    });
}

fn send(writer: &Mutex<TcpStream>, bytes: &[u8]) -> io::Result<()> {
    writer
        .lock()
        .map_err(|_| io::Error::other("Telnet writer mutex poisoned"))?
        .write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_records_can_be_split_across_reads() {
        let mut records = ControlRecords {
            ready: false,
            pending: Vec::new(),
            exit: None,
        };
        assert!(records.push(b"\x1eALFS-TEL").unwrap().is_empty());
        assert!(records.push(b"NET/1 READY\r\n").unwrap().is_empty());
        assert!(records.ready);
        assert!(
            records
                .push(b"\x1eALFS-TELNET/1 EXIT 17\r\n")
                .unwrap()
                .is_empty()
        );
        assert_eq!(records.exit, Some(17));
    }

    #[test]
    fn normal_output_is_preserved() {
        let mut records = ControlRecords {
            ready: true,
            pending: Vec::new(),
            exit: None,
        };
        assert_eq!(records.push(b"hello\r\n").unwrap(), b"hello\r\n");
    }

    #[test]
    fn pre_ready_telnet_preamble_is_suppressed() {
        let mut records = ControlRecords {
            ready: false,
            pending: Vec::new(),
            exit: None,
        };
        assert!(records.push(b"\r\n").unwrap().is_empty());
        assert!(
            records
                .push(b"\x1eALFS-TELNET/1 READY\r\n")
                .unwrap()
                .is_empty()
        );
        assert_eq!(records.push(b"Linux\r\n").unwrap(), b"Linux\r\n");
    }

    #[test]
    fn startup_request_keeps_each_argument() {
        let request = StartupRequest::exec(vec!["printf".into(), "%s".into(), "a b".into()]);
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"mode":"exec","version":1,"argv":["printf","%s","a b"]}"#
        );
    }
}
