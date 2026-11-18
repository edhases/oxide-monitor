//! Monitor-side client for the elevated helper pipe.

use std::time::Duration;

use oxide_ipc::pipe::{PipeConnection, connect_with_retry};
use oxide_ipc::security::PeerPolicy;

use crate::protocol::{
    HelloAck, MAX_MESSAGE, Message, PROTOCOL_VERSION, TempReading, read_message, write_message,
};
use crate::{HELPER_EXE, HostError, pawnio, pipe_name};

pub struct HelperClient {
    conn: PipeConnection,
    hello: HelloAck,
}

impl HelperClient {
    /// Connect to the standard per-user helper pipe, verifying that the server image
    /// is the sibling `oxide-pawnio-host.exe`.
    pub fn connect(timeout: Duration) -> Result<Self, HostError> {
        let pipe = pipe_name()?;
        Self::connect_to(pipe, timeout, PeerPolicy::sibling_exe(HELPER_EXE))
    }

    /// Test/advanced variant with an explicit pipe name and peer policy.
    pub fn connect_to(
        pipe: impl AsRef<str>,
        timeout: Duration,
        policy: PeerPolicy,
    ) -> Result<Self, HostError> {
        let conn = connect_with_retry(pipe.as_ref(), timeout, &policy)?;
        let mut client = Self {
            conn,
            hello: HelloAck {
                protocol: 0,
                pawnio_api: 0,
                module_hash_prefix: 0,
                max_message: 0,
            },
        };
        write_message(
            &client.conn,
            &Message::Hello {
                client_pid: std::process::id(),
                protocol: PROTOCOL_VERSION,
            },
            None,
        )?;
        match read_message(&client.conn, Some(Duration::from_secs(3)))? {
            Message::HelloAck(ack) => {
                if ack.protocol != PROTOCOL_VERSION {
                    return Err(HostError::Protocol(
                        crate::protocol::ProtocolError::BadVersion(ack.protocol),
                    ));
                }
                if ack.module_hash_prefix != pawnio::module_hash_prefix() {
                    return Err(HostError::ModuleRejected(format!(
                        "helper module hash {:#010X} != monitor pin {:#010X}",
                        ack.module_hash_prefix,
                        pawnio::module_hash_prefix()
                    )));
                }
                if ack.max_message < MAX_MESSAGE as u16 {
                    return Err(HostError::Protocol(
                        crate::protocol::ProtocolError::PayloadTooLarge(ack.max_message as usize),
                    ));
                }
                client.hello = ack;
                Ok(client)
            }
            Message::Error { code } => Err(HostError::Protocol(
                crate::protocol::ProtocolError::UnknownKind(code as u16),
            )),
            other => Err(HostError::Protocol(
                crate::protocol::ProtocolError::BadPayload {
                    kind: 0,
                    expected: 0,
                    actual: other.encode().map(|b| b.len()).unwrap_or(0),
                },
            )),
        }
    }

    pub fn hello(&self) -> HelloAck {
        self.hello
    }

    pub fn read_cpu_temp(&self, timeout: Duration) -> Result<TempReading, HostError> {
        write_message(&self.conn, &Message::ReadCpuTemp, None)?;
        match read_message(&self.conn, Some(timeout))? {
            Message::TempReply(reply) => {
                let reading = reply.into_reading().ok_or_else(|| {
                    HostError::PawnIo(format!(
                        "helper returned temperature status {} (raw {:#010X})",
                        reply.status, reply.raw
                    ))
                })?;
                if reading.is_plausible() {
                    Ok(reading)
                } else {
                    Err(HostError::PawnIo(format!(
                        "implausible CPU temperature {:.3} C (raw {:#010X})",
                        reading.temp_c, reading.raw
                    )))
                }
            }
            Message::Error { code } => Err(HostError::PawnIo(format!("helper error code {code}"))),
            other => Err(HostError::PawnIo(format!(
                "unexpected helper reply: {other:?}"
            ))),
        }
    }

    pub fn shutdown(&self) -> Result<(), HostError> {
        write_message(&self.conn, &Message::Shutdown, None)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    use oxide_ipc::pipe::PipeListener;

    use super::*;
    use crate::protocol::{HelloAck, MAX_MESSAGE, TempReply, read_message, write_message};

    fn unique_pipe(tag: &str) -> String {
        static COUNTER: AtomicUsize = AtomicUsize::new(1);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!(
            r"\\.\pipe\oxide-pawnio-test-{}-{tag}-{n}",
            std::process::id()
        )
    }

    fn fake_server(pipe: String, stop: std::sync::Arc<WinEvent>) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let listener = PipeListener::bind(&pipe).unwrap();
            let conn = listener
                .accept(Duration::from_secs(5), &stop, &PeerPolicy::SameProcessOnly)
                .unwrap();
            match read_message(&conn, Some(Duration::from_secs(3))).unwrap() {
                Message::Hello { .. } => {}
                other => panic!("unexpected handshake {other:?}"),
            }
            write_message(
                &conn,
                &Message::HelloAck(HelloAck {
                    protocol: PROTOCOL_VERSION,
                    pawnio_api: 0x0002_0000,
                    module_hash_prefix: pawnio::module_hash_prefix(),
                    max_message: MAX_MESSAGE as u16,
                }),
                None,
            )
            .unwrap();

            for _ in 0..2 {
                match read_message(&conn, Some(Duration::from_secs(3))).unwrap() {
                    Message::ReadCpuTemp => {
                        write_message(
                            &conn,
                            &Message::TempReply(TempReply {
                                status: 0,
                                temp_millic: 72_000,
                                raw: 0x7908_0000,
                                flags: 3,
                            }),
                            None,
                        )
                        .unwrap();
                    }
                    Message::Shutdown => break,
                    other => panic!("unexpected request {other:?}"),
                }
            }
        })
    }

    #[test]
    fn handshake_read_and_shutdown_over_a_real_pipe() {
        let pipe = unique_pipe("roundtrip");
        let stop = std::sync::Arc::new(WinEvent::new_manual_reset().unwrap());
        let server = fake_server(pipe.clone(), std::sync::Arc::clone(&stop));

        let client =
            HelperClient::connect_to(&pipe, Duration::from_secs(5), PeerPolicy::SameProcessOnly)
                .unwrap();
        assert_eq!(client.hello().protocol, PROTOCOL_VERSION);
        let reading = client.read_cpu_temp(Duration::from_secs(2)).unwrap();
        assert!((reading.temp_c - 72.0).abs() < f32::EPSILON);
        client.shutdown().unwrap();
        server.join().unwrap();
    }

    use oxide_ipc::pipe::WinEvent;
}
