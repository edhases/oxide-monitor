//! Fixed-size binary protocol for the elevated PawnIO helper pipe.
//!
//! Every message is at most 64 bytes and starts with a 12-byte header:
//! `magic u32 | version u16 | kind u16 | payload_len u16 | reserved u16`.
//! There is no JSON, no arbitrary function name, and no path/offset supplied by
//! the client: only Hello, ReadCpuTemp, and Shutdown are accepted.

use std::time::Duration;

use oxide_ipc::pipe::PipeConnection;

use crate::HostError;

pub const MAGIC: u32 = 0x4F49_504F; // bytes "OPIO"
pub const PROTOCOL_VERSION: u16 = 1;
pub const HEADER_LEN: usize = 12;
pub const MAX_MESSAGE: usize = 64;
pub const MAX_PAYLOAD: usize = MAX_MESSAGE - HEADER_LEN;

/// SMN offset for AMD family 17h-1Ah `THM_TCON_CUR_TMP` (same value LHM uses).
pub const F17H_M01H_THM_TCON_CUR_TMP: u32 = 0x0005_9800;
/// Bit 19: an additional 49 C offset is present in the raw value.
pub const F17H_TEMP_OFFSET_FLAG: u32 = 0x0008_0000;

pub const TEMP_OK: u32 = 0;
pub const TEMP_UNAVAILABLE: u32 = 1;
pub const TEMP_READ_FAILED: u32 = 2;
pub const TEMP_MODULE_REJECTED: u32 = 3;

const KIND_HELLO: u16 = 1;
const KIND_HELLO_ACK: u16 = 2;
const KIND_READ_CPU_TEMP: u16 = 3;
const KIND_TEMP_REPLY: u16 = 4;
const KIND_SHUTDOWN: u16 = 5;
const KIND_ERROR: u16 = 6;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("message is shorter than the {HEADER_LEN}-byte header")]
    ShortHeader,
    #[error("bad magic {0:#010X}")]
    BadMagic(u32),
    #[error("unsupported protocol version {0}")]
    BadVersion(u16),
    #[error("payload length {0} exceeds {MAX_PAYLOAD}")]
    PayloadTooLarge(usize),
    #[error("message length mismatch: header says {declared}, got {actual}")]
    LengthMismatch { declared: usize, actual: usize },
    #[error("unknown message kind {0}")]
    UnknownKind(u16),
    #[error("invalid payload length {actual} for kind {kind} (expected {expected})")]
    BadPayload {
        kind: u16,
        expected: usize,
        actual: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HelloAck {
    pub protocol: u16,
    /// Value returned by `pawnio_version` (e.g. 0x0002_0000 for API 2.0.0).
    pub pawnio_api: u32,
    /// First four bytes of the pinned module SHA-256, for diagnostics.
    pub module_hash_prefix: u32,
    pub max_message: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TempReply {
    pub status: u32,
    /// Signed millidegrees Celsius (`72_000` = 72.000 C).
    pub temp_millic: i32,
    pub raw: u32,
    pub flags: u32,
}

impl TempReply {
    pub fn from_reading(reading: TempReading) -> Self {
        Self {
            status: TEMP_OK,
            temp_millic: reading.temp_millic(),
            raw: reading.raw,
            flags: reading.flags(),
        }
    }

    pub fn error(status: u32) -> Self {
        Self {
            status,
            temp_millic: 0,
            raw: 0,
            flags: 0,
        }
    }

    pub fn into_reading(self) -> Option<TempReading> {
        (self.status == TEMP_OK).then(|| TempReading {
            temp_c: self.temp_millic as f32 / 1000.0,
            raw: self.raw,
            offset_flag: self.flags & 1 != 0,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Message {
    Hello { client_pid: u32, protocol: u16 },
    HelloAck(HelloAck),
    ReadCpuTemp,
    TempReply(TempReply),
    Shutdown,
    Error { code: u32 },
}

impl Message {
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        let (kind, payload): (u16, Vec<u8>) = match self {
            Self::Hello {
                client_pid,
                protocol,
            } => {
                let mut p = Vec::with_capacity(8);
                p.extend_from_slice(&client_pid.to_le_bytes());
                p.extend_from_slice(&protocol.to_le_bytes());
                p.extend_from_slice(&0u16.to_le_bytes());
                (KIND_HELLO, p)
            }
            Self::HelloAck(ack) => {
                let mut p = Vec::with_capacity(16);
                p.extend_from_slice(&ack.protocol.to_le_bytes());
                p.extend_from_slice(&0u16.to_le_bytes());
                p.extend_from_slice(&ack.pawnio_api.to_le_bytes());
                p.extend_from_slice(&ack.module_hash_prefix.to_le_bytes());
                p.extend_from_slice(&ack.max_message.to_le_bytes());
                p.extend_from_slice(&0u16.to_le_bytes());
                (KIND_HELLO_ACK, p)
            }
            Self::ReadCpuTemp => (KIND_READ_CPU_TEMP, Vec::new()),
            Self::TempReply(reply) => {
                let mut p = Vec::with_capacity(16);
                p.extend_from_slice(&reply.status.to_le_bytes());
                p.extend_from_slice(&reply.temp_millic.to_le_bytes());
                p.extend_from_slice(&reply.raw.to_le_bytes());
                p.extend_from_slice(&reply.flags.to_le_bytes());
                (KIND_TEMP_REPLY, p)
            }
            Self::Shutdown => (KIND_SHUTDOWN, Vec::new()),
            Self::Error { code } => (KIND_ERROR, code.to_le_bytes().to_vec()),
        };

        if payload.len() > MAX_PAYLOAD {
            return Err(ProtocolError::PayloadTooLarge(payload.len()));
        }

        let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&payload);
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() < HEADER_LEN {
            return Err(ProtocolError::ShortHeader);
        }
        let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        if magic != MAGIC {
            return Err(ProtocolError::BadMagic(magic));
        }
        let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
        if version != PROTOCOL_VERSION {
            return Err(ProtocolError::BadVersion(version));
        }
        let kind = u16::from_le_bytes(bytes[6..8].try_into().unwrap());
        let declared = u16::from_le_bytes(bytes[8..10].try_into().unwrap()) as usize;
        if declared > MAX_PAYLOAD {
            return Err(ProtocolError::PayloadTooLarge(declared));
        }
        if bytes.len() != HEADER_LEN + declared {
            return Err(ProtocolError::LengthMismatch {
                declared,
                actual: bytes.len() - HEADER_LEN,
            });
        }
        let p = &bytes[HEADER_LEN..];

        let expect = |expected: usize| -> Result<(), ProtocolError> {
            if p.len() == expected {
                Ok(())
            } else {
                Err(ProtocolError::BadPayload {
                    kind,
                    expected,
                    actual: p.len(),
                })
            }
        };

        match kind {
            KIND_HELLO => {
                expect(8)?;
                Ok(Self::Hello {
                    client_pid: u32::from_le_bytes(p[0..4].try_into().unwrap()),
                    protocol: u16::from_le_bytes(p[4..6].try_into().unwrap()),
                })
            }
            KIND_HELLO_ACK => {
                expect(16)?;
                Ok(Self::HelloAck(HelloAck {
                    protocol: u16::from_le_bytes(p[0..2].try_into().unwrap()),
                    pawnio_api: u32::from_le_bytes(p[4..8].try_into().unwrap()),
                    module_hash_prefix: u32::from_le_bytes(p[8..12].try_into().unwrap()),
                    max_message: u16::from_le_bytes(p[12..14].try_into().unwrap()),
                }))
            }
            KIND_READ_CPU_TEMP => {
                expect(0)?;
                Ok(Self::ReadCpuTemp)
            }
            KIND_TEMP_REPLY => {
                expect(16)?;
                Ok(Self::TempReply(TempReply {
                    status: u32::from_le_bytes(p[0..4].try_into().unwrap()),
                    temp_millic: i32::from_le_bytes(p[4..8].try_into().unwrap()),
                    raw: u32::from_le_bytes(p[8..12].try_into().unwrap()),
                    flags: u32::from_le_bytes(p[12..16].try_into().unwrap()),
                }))
            }
            KIND_SHUTDOWN => {
                expect(0)?;
                Ok(Self::Shutdown)
            }
            KIND_ERROR => {
                expect(4)?;
                Ok(Self::Error {
                    code: u32::from_le_bytes(p[0..4].try_into().unwrap()),
                })
            }
            other => Err(ProtocolError::UnknownKind(other)),
        }
    }
}

/// Decode `THM_TCON_CUR_TMP` exactly like LHM/k10temp.
pub fn decode_temp(raw: u32) -> TempReading {
    let mut temp_c = ((raw >> 21) as f32) * 0.125;
    let offset_flag = raw & F17H_TEMP_OFFSET_FLAG != 0;
    if offset_flag {
        temp_c -= 49.0;
    }
    TempReading {
        temp_c,
        raw,
        offset_flag,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TempReading {
    pub temp_c: f32,
    pub raw: u32,
    pub offset_flag: bool,
}

impl TempReading {
    pub fn temp_millic(self) -> i32 {
        (self.temp_c * 1000.0).round() as i32
    }

    pub fn flags(self) -> u32 {
        u32::from(self.offset_flag) | (u32::from(self.is_plausible()) << 1)
    }

    /// Reject zeroed/unmapped registers and obviously bogus values. A real CPU
    /// Tctl/Tdie is roughly 20-105 C; allow a wide 5-125 C window.
    pub fn is_plausible(self) -> bool {
        self.temp_c.is_finite() && (5.0..=125.0).contains(&self.temp_c)
    }
}

/// Read one fixed-size message from a pipe endpoint with a bounded timeout.
pub fn read_message(
    conn: &PipeConnection,
    timeout: Option<Duration>,
) -> Result<Message, HostError> {
    let mut header = [0u8; HEADER_LEN];
    conn.read_exact_timeout(&mut header, timeout)?;
    let declared = u16::from_le_bytes(header[8..10].try_into().unwrap()) as usize;
    if declared > MAX_PAYLOAD {
        return Err(ProtocolError::PayloadTooLarge(declared).into());
    }
    let mut bytes = [0u8; MAX_MESSAGE];
    bytes[..HEADER_LEN].copy_from_slice(&header);
    if declared > 0 {
        conn.read_exact_timeout(
            &mut bytes[HEADER_LEN..HEADER_LEN + declared],
            Some(Duration::from_secs(2)),
        )?;
    }
    Ok(Message::decode(&bytes[..HEADER_LEN + declared])?)
}

pub fn write_message(
    conn: &PipeConnection,
    msg: &Message,
    stop: Option<&oxide_ipc::pipe::WinEvent>,
) -> Result<(), HostError> {
    let bytes = msg.encode()?;
    conn.write_all(&bytes, stop)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_message_roundtrips_and_stays_bounded() {
        let messages = [
            Message::Hello {
                client_pid: 42,
                protocol: PROTOCOL_VERSION,
            },
            Message::HelloAck(HelloAck {
                protocol: PROTOCOL_VERSION,
                pawnio_api: 0x0002_0000,
                module_hash_prefix: 0x099D_C01D,
                max_message: MAX_MESSAGE as u16,
            }),
            Message::ReadCpuTemp,
            Message::TempReply(TempReply {
                status: TEMP_OK,
                temp_millic: 72_000,
                raw: 0x7908_0000,
                flags: 3,
            }),
            Message::Shutdown,
            Message::Error { code: 7 },
        ];
        for msg in messages {
            let bytes = msg.encode().unwrap();
            assert!(bytes.len() <= MAX_MESSAGE);
            assert_eq!(Message::decode(&bytes).unwrap(), msg);
        }
    }

    #[test]
    fn malformed_messages_are_rejected() {
        let mut bytes = Message::ReadCpuTemp.encode().unwrap();
        bytes[0] ^= 0xFF;
        assert!(matches!(
            Message::decode(&bytes),
            Err(ProtocolError::BadMagic(_))
        ));

        let mut bytes = Message::ReadCpuTemp.encode().unwrap();
        bytes[4] = 99;
        assert!(matches!(
            Message::decode(&bytes),
            Err(ProtocolError::BadVersion(99))
        ));

        let mut bytes = Message::ReadCpuTemp.encode().unwrap();
        bytes[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(matches!(
            Message::decode(&bytes),
            Err(ProtocolError::PayloadTooLarge(_))
        ));

        assert!(matches!(
            Message::decode(&[0u8; HEADER_LEN - 1]),
            Err(ProtocolError::ShortHeader)
        ));
    }

    #[test]
    fn temperature_decode_applies_the_49c_offset_flag() {
        let hot = decode_temp(0x7908_0000);
        assert!(hot.offset_flag);
        assert!((hot.temp_c - 72.0).abs() < f32::EPSILON, "{hot:?}");
        assert_eq!(hot.temp_millic(), 72_000);
        assert!(hot.is_plausible());

        let cold = decode_temp(0x1900_0000);
        assert!(!cold.offset_flag);
        assert!((cold.temp_c - 25.0).abs() < f32::EPSILON, "{cold:?}");
        assert!(cold.is_plausible());

        assert!(!decode_temp(0).is_plausible());
        assert!(!decode_temp(u32::MAX).is_plausible());
    }

    #[test]
    fn temp_reply_roundtrips_the_reading() {
        let reading = decode_temp(0x7908_0000);
        let reply = TempReply::from_reading(reading);
        assert_eq!(reply.status, TEMP_OK);
        assert_eq!(reply.temp_millic, 72_000);
        let back = reply.into_reading().unwrap();
        assert!((back.temp_c - 72.0).abs() < f32::EPSILON);
        assert_eq!(back.raw, reading.raw);
        assert_eq!(TempReply::error(TEMP_READ_FAILED).into_reading(), None);
    }
}
