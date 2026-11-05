//! Streaming Telnet framing. Negotiation bytes never reach the browser terminal.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;
const BINARY: u8 = 0;
const ECHO: u8 = 1;
const SGA: u8 = 3;
const TTYPE: u8 = 24;
const NAWS: u8 = 31;
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
enum DecodeState {
    #[default]
    Data,
    Command,
    Option(u8),
    Subnegotiation,
    SubnegotiationCommand,
}

pub(crate) struct Connection {
    stream: TcpStream,
    state: DecodeState,
    subnegotiation: Vec<u8>,
    local_options: [bool; 256],
    peer_options: [bool; 256],
    columns: u16,
    rows: u16,
    carriage_return: bool,
}

impl Connection {
    pub(crate) fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            state: DecodeState::Data,
            subnegotiation: Vec::new(),
            local_options: [false; 256],
            peer_options: [false; 256],
            columns: 80,
            rows: 24,
            carriage_return: false,
        }
    }

    /// A chunk can contain only negotiation. None means EOF, not empty output.
    pub(crate) async fn read(&mut self) -> Result<Option<Vec<u8>>> {
        let mut bytes = [0; 8192];
        let count = self.stream.read(&mut bytes).await?;
        if count == 0 {
            return Ok(None);
        }
        let mut data = Vec::with_capacity(count);
        let mut reply = Vec::new();
        for &byte in &bytes[..count] {
            match self.state {
                DecodeState::Data if byte == IAC => self.state = DecodeState::Command,
                DecodeState::Data => {
                    // NVT CR NUL is one literal CR. Preserve CR LF for the terminal.
                    if !(self.carriage_return && byte == 0 && !self.peer_options[BINARY as usize]) {
                        data.push(byte);
                    }
                    self.carriage_return = byte == b'\r';
                }
                DecodeState::Command => {
                    self.state = match byte {
                        IAC => {
                            data.push(IAC);
                            DecodeState::Data
                        }
                        WILL | WONT | DO | DONT => DecodeState::Option(byte),
                        SB => {
                            self.subnegotiation.clear();
                            DecodeState::Subnegotiation
                        }
                        _ => DecodeState::Data,
                    };
                }
                DecodeState::Option(command) => {
                    self.negotiate(command, byte, &mut reply);
                    self.state = DecodeState::Data;
                }
                DecodeState::Subnegotiation if byte == IAC => {
                    self.state = DecodeState::SubnegotiationCommand;
                }
                DecodeState::Subnegotiation => self.subnegotiation.push(byte),
                DecodeState::SubnegotiationCommand => match byte {
                    SE => {
                        if self.subnegotiation == [TTYPE, 1] && self.local_options[TTYPE as usize] {
                            reply.extend_from_slice(&[IAC, SB, TTYPE, 0]);
                            reply.extend_from_slice(b"xterm-256color");
                            reply.extend_from_slice(&[IAC, SE]);
                        }
                        self.subnegotiation.clear();
                        self.state = DecodeState::Data;
                    }
                    IAC => {
                        self.subnegotiation.push(IAC);
                        self.state = DecodeState::Subnegotiation;
                    }
                    _ => self.state = DecodeState::Subnegotiation,
                },
            }
            if self.subnegotiation.len() > 1024 {
                bail!("Telnet negotiation is too large");
            }
        }
        self.write_wire(&reply).await?;
        Ok(Some(data))
    }

    fn negotiate(&mut self, command: u8, option: u8, reply: &mut Vec<u8>) {
        let index = option as usize;
        match command {
            WILL => {
                let supported = matches!(option, BINARY | ECHO | SGA);
                if !self.peer_options[index] {
                    reply.extend_from_slice(&[IAC, if supported { DO } else { DONT }, option]);
                }
                self.peer_options[index] = supported;
            }
            WONT => {
                if self.peer_options[index] {
                    reply.extend_from_slice(&[IAC, DONT, option]);
                }
                self.peer_options[index] = false;
            }
            DO => {
                let supported = matches!(option, BINARY | SGA | TTYPE | NAWS);
                if !self.local_options[index] {
                    reply.extend_from_slice(&[IAC, if supported { WILL } else { WONT }, option]);
                    if supported && option == NAWS {
                        reply.extend(self.window_size());
                    }
                }
                self.local_options[index] = supported;
            }
            DONT => {
                if self.local_options[index] {
                    reply.extend_from_slice(&[IAC, WONT, option]);
                }
                self.local_options[index] = false;
            }
            _ => {}
        }
    }

    pub(crate) async fn input(&mut self, data: &[u8]) -> Result<()> {
        let mut wire = Vec::with_capacity(data.len() * 2);
        let mut position = 0;
        while position < data.len() {
            let byte = data[position];
            if byte == IAC {
                wire.extend_from_slice(&[IAC, IAC]);
            } else if !self.local_options[BINARY as usize] && matches!(byte, b'\r' | b'\n') {
                wire.extend_from_slice(b"\r\n");
                if byte == b'\r' && data.get(position + 1) == Some(&b'\n') {
                    position += 1;
                }
            } else {
                wire.push(byte);
            }
            position += 1;
        }
        self.write_wire(&wire).await
    }

    pub(crate) async fn resize(&mut self, columns: u16, rows: u16) -> Result<()> {
        self.columns = columns.clamp(10, 500);
        self.rows = rows.clamp(5, 200);
        if self.local_options[NAWS as usize] {
            self.write_wire(&self.window_size()).await?;
        }
        Ok(())
    }

    fn window_size(&self) -> Vec<u8> {
        let mut reply = vec![IAC, SB, NAWS];
        for byte in self
            .columns
            .to_be_bytes()
            .into_iter()
            .chain(self.rows.to_be_bytes())
        {
            reply.push(byte);
            if byte == IAC {
                reply.push(IAC);
            }
        }
        reply.extend_from_slice(&[IAC, SE]);
        reply
    }

    async fn write_wire(&mut self, data: &[u8]) -> Result<()> {
        if !data.is_empty() {
            timeout(WRITE_TIMEOUT, self.stream.write_all(data))
                .await
                .context("Telnet write timed out")??;
        }
        Ok(())
    }
}
