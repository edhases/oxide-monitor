//! Overlapped Win32 Named Pipe listener, client connector, and safe interruptible stream.

use std::ffi::c_void;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING,
    ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE,
    GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE,
    FlushFileBuffers, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT, WaitNamedPipeW,
};
use windows::Win32::System::Threading::{
    CreateEventW, INFINITE, ResetEvent, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};
use windows::core::PCWSTR;

use crate::IpcError;
use crate::protocol::decode_frame_len;
use crate::security::{
    CurrentUserDacl, PeerPolicy, to_wide, verify_client_peer, verify_server_peer,
};

const PIPE_BUFFER_SIZE: u32 = 64 * 1024;
const PIPE_DEFAULT_TIMEOUT_MS: u32 = 1_000;

/// Owned Win32 kernel handle closed automatically on `Drop`.
pub struct OwnedHandle(HANDLE);

// SAFETY: Win32 kernel handles (events, pipes) can safely be transferred across threads.
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

impl OwnedHandle {
    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
            self.0 = INVALID_HANDLE_VALUE;
        }
    }
}

/// Manual-reset Win32 Event for zero-polling thread wakeup and cancellation.
pub struct WinEvent(OwnedHandle);

impl WinEvent {
    pub fn new_manual_reset() -> Result<Self, IpcError> {
        let handle = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
            .map_err(|err| IpcError::win32("CreateEventW", err))?;
        Ok(Self(OwnedHandle(handle)))
    }

    pub fn signal(&self) {
        unsafe {
            let _ = SetEvent(self.0.raw());
        }
    }

    pub fn reset(&self) {
        unsafe {
            let _ = ResetEvent(self.0.raw());
        }
    }

    pub fn raw(&self) -> HANDLE {
        self.0.raw()
    }
}

/// Hardened single-instance Named Pipe listener (`FILE_FLAG_FIRST_PIPE_INSTANCE`, current-user DACL,
/// `PIPE_REJECT_REMOTE_CLIENTS`).
pub struct PipeListener {
    handle: OwnedHandle,
    pipe_name: String,
}

impl PipeListener {
    pub fn bind(pipe_name: &str) -> Result<Self, IpcError> {
        let dacl = CurrentUserDacl::build()?;
        Self::bind_with_dacl(pipe_name, &dacl)
    }

    /// Bind a single-instance pipe with an explicit DACL/label. The elevated PawnIO
    /// helper uses `CurrentUserDacl::build_for_medium_integrity()` so the unelevated
    /// monitor is not blocked by the automatic no-write-up rule.
    pub fn bind_with_dacl(pipe_name: &str, dacl: &CurrentUserDacl) -> Result<Self, IpcError> {
        let sa = dacl.security_attributes();
        let name_w = to_wide(pipe_name);

        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(name_w.as_ptr()),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                PIPE_BUFFER_SIZE,
                PIPE_BUFFER_SIZE,
                PIPE_DEFAULT_TIMEOUT_MS,
                Some(&sa),
            )
        };

        if handle == INVALID_HANDLE_VALUE || handle.is_invalid() {
            let err = unsafe { GetLastError() };
            if err == ERROR_ACCESS_DENIED || err == ERROR_PIPE_BUSY {
                return Err(IpcError::PipeAlreadyInUse(pipe_name.to_string()));
            }
            return Err(IpcError::Win32 {
                op: "CreateNamedPipeW",
                detail: format!("error code {}", err.0),
            });
        }

        Ok(Self {
            handle: OwnedHandle(handle),
            pipe_name: pipe_name.to_string(),
        })
    }

    pub fn pipe_name(&self) -> &str {
        &self.pipe_name
    }

    /// Wait for a single client to connect (bounded by `timeout` and interruptible via `stop_event`),
    /// then verify the client executable against `policy`.
    pub fn accept(
        self,
        timeout: Duration,
        stop_event: &WinEvent,
        policy: &PeerPolicy,
    ) -> Result<PipeConnection, IpcError> {
        let connect_event = WinEvent::new_manual_reset()?;
        let mut ov = Box::new(OVERLAPPED {
            hEvent: connect_event.raw(),
            ..Default::default()
        });

        let connected_now = match unsafe { ConnectNamedPipe(self.handle.raw(), Some(&mut *ov)) } {
            Ok(()) => true,
            Err(_) => {
                let err = unsafe { GetLastError() };
                if err == ERROR_PIPE_CONNECTED {
                    true
                } else if err == ERROR_IO_PENDING {
                    false
                } else {
                    return Err(IpcError::Win32 {
                        op: "ConnectNamedPipe",
                        detail: format!("error code {}", err.0),
                    });
                }
            }
        };

        if !connected_now {
            let handles = [connect_event.raw(), stop_event.raw()];
            let wait_ms = timeout.as_millis().min(u32::MAX as u128) as u32;
            let wait = unsafe { WaitForMultipleObjects(&handles, false, wait_ms) };
            if wait == WAIT_OBJECT_0 {
                let mut transferred = 0u32;
                unsafe {
                    GetOverlappedResult(self.handle.raw(), &*ov, &mut transferred, false)
                        .map_err(|err| IpcError::win32("GetOverlappedResult(connect)", err))?;
                }
            } else {
                unsafe {
                    let _ = CancelIoEx(self.handle.raw(), Some(&*ov));
                    let mut transferred = 0u32;
                    let _ = GetOverlappedResult(self.handle.raw(), &*ov, &mut transferred, true);
                }
                if wait == WAIT_TIMEOUT {
                    return Err(IpcError::Timeout);
                }
                return Err(IpcError::Stopped);
            }
        }

        let peer_pid = verify_client_peer(self.handle.raw(), policy)?;
        Ok(PipeConnection {
            handle: self.handle,
            is_server: true,
            peer_pid,
            write_event: WinEvent::new_manual_reset()?,
        })
    }
}

/// Connect to an existing Named Pipe with retry loop up to `timeout`, verifying the server image.
pub fn connect_with_retry(
    pipe_name: &str,
    timeout: Duration,
    policy: &PeerPolicy,
) -> Result<PipeConnection, IpcError> {
    let deadline = Instant::now() + timeout;
    let name_w = to_wide(pipe_name);

    loop {
        let res = unsafe {
            CreateFileW(
                PCWSTR(name_w.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
        };

        match res {
            Ok(handle) if !handle.is_invalid() => {
                let owned = OwnedHandle(handle);
                let peer_pid = verify_server_peer(owned.raw(), policy)?;
                return Ok(PipeConnection {
                    handle: owned,
                    is_server: false,
                    peer_pid,
                    write_event: WinEvent::new_manual_reset()?,
                });
            }
            Ok(_) | Err(_) => {
                let now = Instant::now();
                if now >= deadline {
                    return Err(IpcError::Timeout);
                }
                let rem_ms = deadline
                    .saturating_duration_since(now)
                    .as_millis()
                    .clamp(1, 50) as u32;
                let waited = unsafe { WaitNamedPipeW(PCWSTR(name_w.as_ptr()), rem_ms) }.as_bool();
                if !waited {
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
    }
}

/// Connected overlapped Named Pipe endpoint (either server-side or client-side).
pub struct PipeConnection {
    handle: OwnedHandle,
    is_server: bool,
    peer_pid: u32,
    write_event: WinEvent,
}

impl PipeConnection {
    pub fn peer_pid(&self) -> u32 {
        self.peer_pid
    }

    pub fn raw(&self) -> HANDLE {
        self.handle.raw()
    }

    pub fn flush(&self) {
        unsafe {
            let _ = FlushFileBuffers(self.handle.raw());
        }
    }

    /// Write a full framed buffer (`u32 LE` + payload) using overlapped I/O.
    pub fn write_all(&self, data: &[u8], stop_event: Option<&WinEvent>) -> Result<(), IpcError> {
        let mut offset = 0usize;
        while offset < data.len() {
            self.write_event.reset();
            let mut ov = OVERLAPPED {
                hEvent: self.write_event.raw(),
                ..Default::default()
            };
            let slice = &data[offset..];
            // On FILE_FLAG_OVERLAPPED handles, pass None for lpNumberOfBytesWritten and read
            // the exact transferred count via GetOverlappedResult.
            let immediate =
                unsafe { WriteFile(self.handle.raw(), Some(slice), None, Some(&mut ov)) };

            match immediate {
                Ok(()) => {
                    let mut transferred = 0u32;
                    unsafe {
                        GetOverlappedResult(self.handle.raw(), &ov, &mut transferred, true)
                            .map_err(map_pipe_io_error)?;
                    }
                    if transferred == 0 {
                        return Err(IpcError::Disconnected);
                    }
                    offset += transferred as usize;
                }
                Err(_) => {
                    let err = unsafe { GetLastError() };
                    if err == ERROR_IO_PENDING {
                        let wait = if let Some(stop) = stop_event {
                            let handles = [self.write_event.raw(), stop.raw()];
                            unsafe { WaitForMultipleObjects(&handles, false, 5_000) }
                        } else {
                            unsafe { WaitForSingleObject(self.write_event.raw(), 5_000) }
                        };
                        if wait == WAIT_OBJECT_0 {
                            let mut transferred = 0u32;
                            unsafe {
                                GetOverlappedResult(
                                    self.handle.raw(),
                                    &ov,
                                    &mut transferred,
                                    false,
                                )
                                .map_err(map_pipe_io_error)?;
                            }
                            if transferred == 0 {
                                return Err(IpcError::Disconnected);
                            }
                            offset += transferred as usize;
                        } else {
                            unsafe {
                                let _ = CancelIoEx(self.handle.raw(), Some(&ov));
                                let mut transferred = 0u32;
                                let _ = GetOverlappedResult(
                                    self.handle.raw(),
                                    &ov,
                                    &mut transferred,
                                    true,
                                );
                            }
                            if wait == WAIT_TIMEOUT {
                                return Err(IpcError::Timeout);
                            }
                            return Err(IpcError::Stopped);
                        }
                    } else if err == ERROR_BROKEN_PIPE || err == ERROR_PIPE_NOT_CONNECTED {
                        return Err(IpcError::Disconnected);
                    } else {
                        return Err(IpcError::Win32 {
                            op: "WriteFile",
                            detail: format!("error code {}", err.0),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// Read exactly `buf.len()` bytes with an optional timeout. Used by the
    /// fixed-size PawnIO helper protocol (the stream is not JSON-framed).
    pub fn read_exact_timeout(
        &self,
        buf: &mut [u8],
        timeout: Option<Duration>,
    ) -> Result<(), IpcError> {
        let read_event = WinEvent::new_manual_reset()?;
        self.read_exact_timeout_inner(buf, &read_event, timeout)
    }

    /// Blocking (overlapped-backed) framed read with optional timeout, used by `IpcClient`.
    pub fn read_frame_timeout(&self, timeout: Option<Duration>) -> Result<Vec<u8>, IpcError> {
        let read_event = WinEvent::new_manual_reset()?;
        let mut header = [0u8; 4];
        self.read_exact_timeout_inner(&mut header, &read_event, timeout)?;
        let len = decode_frame_len(header)?;
        let mut payload = vec![0u8; len];
        // Once header arrived, give generous time to finish reading the body.
        self.read_exact_timeout_inner(&mut payload, &read_event, Some(Duration::from_secs(5)))?;
        Ok(payload)
    }

    fn read_exact_timeout_inner(
        &self,
        buf: &mut [u8],
        read_event: &WinEvent,
        timeout: Option<Duration>,
    ) -> Result<(), IpcError> {
        let mut offset = 0usize;
        let deadline = timeout.map(|d| Instant::now() + d);

        while offset < buf.len() {
            read_event.reset();
            let mut ov = OVERLAPPED {
                hEvent: read_event.raw(),
                ..Default::default()
            };
            let res = unsafe {
                ReadFile(
                    self.handle.raw(),
                    Some(&mut buf[offset..]),
                    None,
                    Some(&mut ov),
                )
            };
            match res {
                Ok(()) => {
                    let mut transferred = 0u32;
                    unsafe {
                        GetOverlappedResult(self.handle.raw(), &ov, &mut transferred, true)
                            .map_err(map_pipe_io_error)?;
                    }
                    if transferred == 0 {
                        return Err(IpcError::Disconnected);
                    }
                    offset += transferred as usize;
                }
                Err(_) => {
                    let err = unsafe { GetLastError() };
                    if err == ERROR_IO_PENDING {
                        let wait_ms = match deadline {
                            Some(dl) => {
                                let rem = dl.saturating_duration_since(Instant::now());
                                if rem.is_zero() {
                                    unsafe {
                                        let _ = CancelIoEx(self.handle.raw(), Some(&ov));
                                        let mut t = 0u32;
                                        let _ = GetOverlappedResult(
                                            self.handle.raw(),
                                            &ov,
                                            &mut t,
                                            true,
                                        );
                                    }
                                    return Err(IpcError::Timeout);
                                }
                                rem.as_millis().min(u32::MAX as u128) as u32
                            }
                            None => INFINITE,
                        };
                        let wait = unsafe { WaitForSingleObject(read_event.raw(), wait_ms) };
                        if wait == WAIT_OBJECT_0 {
                            let mut transferred = 0u32;
                            unsafe {
                                GetOverlappedResult(
                                    self.handle.raw(),
                                    &ov,
                                    &mut transferred,
                                    false,
                                )
                                .map_err(map_pipe_io_error)?;
                            }
                            if transferred == 0 {
                                return Err(IpcError::Disconnected);
                            }
                            offset += transferred as usize;
                        } else {
                            unsafe {
                                let _ = CancelIoEx(self.handle.raw(), Some(&ov));
                                let mut t = 0u32;
                                let _ = GetOverlappedResult(self.handle.raw(), &ov, &mut t, true);
                            }
                            return Err(IpcError::Timeout);
                        }
                    } else if err == ERROR_BROKEN_PIPE || err == ERROR_PIPE_NOT_CONNECTED {
                        return Err(IpcError::Disconnected);
                    } else {
                        return Err(IpcError::Win32 {
                            op: "ReadFile",
                            detail: format!("error code {}", err.0),
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

impl Drop for PipeConnection {
    fn drop(&mut self) {
        if self.is_server && !self.handle.raw().is_invalid() {
            unsafe {
                let _ = DisconnectNamedPipe(self.handle.raw());
            }
        }
    }
}

/// Safe single-thread overlapped reader state machine multiplexed with `send_event` and `stop_event`.
///
/// Ensures `OVERLAPPED` and its heap-allocated buffer remain pinned and valid until either
/// completion or explicit `CancelIoEx` + `GetOverlappedResult(..., true)`.
pub struct MultiplexedFrameReader {
    pipe: HANDLE,
    read_event: WinEvent,
    ov: Box<OVERLAPPED>,
    pending: bool,
    reading_payload: bool,
    header_buf: Box<[u8; 4]>,
    payload_buf: Vec<u8>,
    offset: usize,
}

impl MultiplexedFrameReader {
    pub fn new(pipe: HANDLE) -> Result<Self, IpcError> {
        let read_event = WinEvent::new_manual_reset()?;
        let ov = Box::new(OVERLAPPED {
            hEvent: read_event.raw(),
            ..Default::default()
        });
        Ok(Self {
            pipe,
            read_event,
            ov,
            pending: false,
            reading_payload: false,
            header_buf: Box::new([0u8; 4]),
            payload_buf: Vec::new(),
            offset: 0,
        })
    }

    pub fn event_handle(&self) -> HANDLE {
        self.read_event.raw()
    }

    /// Drive reads until either a complete payload frame is ready or an overlapped `ReadFile`
    /// is pending (`Ok(None)`).
    pub fn poll_or_arm(&mut self) -> Result<Option<Vec<u8>>, IpcError> {
        loop {
            if self.pending {
                let mut transferred = 0u32;
                let res =
                    unsafe { GetOverlappedResult(self.pipe, &*self.ov, &mut transferred, false) };
                match res {
                    Ok(()) => {
                        self.pending = false;
                        if transferred == 0 {
                            return Err(IpcError::Disconnected);
                        }
                        self.offset += transferred as usize;
                    }
                    Err(err) => {
                        let code = unsafe { GetLastError() };
                        if code == ERROR_IO_INCOMPLETE || code == ERROR_IO_PENDING {
                            return Ok(None);
                        }
                        self.pending = false;
                        return Err(map_pipe_io_error(err));
                    }
                }
            }

            if !self.reading_payload {
                if self.offset == 4 {
                    let len = decode_frame_len(*self.header_buf)?;
                    self.payload_buf = vec![0u8; len];
                    self.reading_payload = true;
                    self.offset = 0;
                }
            } else if self.offset == self.payload_buf.len() {
                let finished = std::mem::take(&mut self.payload_buf);
                self.reading_payload = false;
                self.offset = 0;
                return Ok(Some(finished));
            }

            // Issue the next ReadFile on the active target buffer.
            self.read_event.reset();
            *self.ov = OVERLAPPED {
                hEvent: self.read_event.raw(),
                ..Default::default()
            };
            let target: &mut [u8] = if self.reading_payload {
                &mut self.payload_buf[self.offset..]
            } else {
                &mut self.header_buf[self.offset..]
            };

            let res = unsafe { ReadFile(self.pipe, Some(target), None, Some(&mut *self.ov)) };
            match res {
                Ok(()) => {
                    let mut transferred = 0u32;
                    unsafe {
                        GetOverlappedResult(self.pipe, &*self.ov, &mut transferred, true)
                            .map_err(map_pipe_io_error)?;
                    }
                    if transferred == 0 {
                        return Err(IpcError::Disconnected);
                    }
                    self.offset += transferred as usize;
                }
                Err(_) => {
                    let err = unsafe { GetLastError() };
                    if err == ERROR_IO_PENDING {
                        self.pending = true;
                        return Ok(None);
                    }
                    if err == ERROR_BROKEN_PIPE || err == ERROR_PIPE_NOT_CONNECTED {
                        return Err(IpcError::Disconnected);
                    }
                    return Err(IpcError::Win32 {
                        op: "ReadFile",
                        detail: format!("error code {}", err.0),
                    });
                }
            }
        }
    }
}

impl Drop for MultiplexedFrameReader {
    fn drop(&mut self) {
        if self.pending && !self.pipe.is_invalid() {
            unsafe {
                let _ = CancelIoEx(self.pipe, Some(&*self.ov));
                let mut transferred = 0u32;
                let _ = GetOverlappedResult(self.pipe, &*self.ov, &mut transferred, true);
            }
            self.pending = false;
        }
    }
}

fn map_pipe_io_error(err: windows::core::Error) -> IpcError {
    let raw = (err.code().0 as u32) & 0xFFFF;
    if raw == ERROR_BROKEN_PIPE.0 || raw == ERROR_PIPE_NOT_CONNECTED.0 {
        IpcError::Disconnected
    } else {
        IpcError::Win32 {
            op: "GetOverlappedResult",
            detail: err.to_string(),
        }
    }
}

#[allow(dead_code)]
fn _assert_send_sync() {
    fn is_send<T: Send>() {}
    fn is_sync<T: Sync>() {}
    is_send::<OwnedHandle>();
    is_sync::<OwnedHandle>();
    is_send::<WinEvent>();
    is_sync::<WinEvent>();
    let _ = std::ptr::null::<c_void>();
}
